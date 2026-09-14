"""Tests for the server CLI's streaming-flag validation, adapter dispatch and
provider.env sharing.

The re-decode tuning flags are whisper-only (contracts/strategy-config.md) and
misuse only warns, so the log is the whole signal - assert on it.
"""

import contextlib
import logging

import pytest

from myna.server.cli import _validate_streaming_args, build_adapter, build_parser, main


def parse(*extra):
    return build_parser().parse_args(["--socket", "/run/myna.sock", *extra])


def test_tuning_flags_warn_when_streaming_is_off(caplog):
    with caplog.at_level(logging.WARNING):
        _validate_streaming_args(parse("--stream-cadence-s", "2"))

    assert "--stream-cadence-s" in caplog.text
    assert "ignored without --streaming" in caplog.text


def test_tuning_flags_warn_for_adapters_that_do_not_read_them(caplog):
    with caplog.at_level(logging.WARNING):
        _validate_streaming_args(
            parse("--streaming", "--adapter", "parakeet", "--stream-beam-size", "5")
        )

    assert "--stream-beam-size" in caplog.text
    assert "whisper-only" in caplog.text
    assert "parakeet" in caplog.text


def test_tuning_flags_are_silent_for_streaming_whisper(caplog):
    with caplog.at_level(logging.WARNING):
        _validate_streaming_args(parse("--streaming", "--stream-window-cap-s", "10"))

    assert caplog.text == ""


def test_no_tuning_flags_is_silent(caplog):
    with caplog.at_level(logging.WARNING):
        _validate_streaming_args(parse())

    assert caplog.text == ""


def test_fake_adapter_builds_without_any_extra():
    from myna.testbed.fake import FakeAdapter

    assert isinstance(build_adapter(parse("--adapter", "fake")), FakeAdapter)


def test_qwen_c_refuses_to_start_without_a_model_dir():
    with pytest.raises(SystemExit, match="requires --model"):
        build_adapter(parse("--adapter", "qwen-c"))


def test_whisper_streaming_defaults_are_the_measured_ones():
    """The re-decode cadence is a measured value, not a taste.

    Whisper's encoder costs the same per call whatever the window holds (a
    fixed 30 s of padded mel), so streaming cost is ticks x a constant and the
    cadence is the only lever on it. 2.0 was chosen over the original 1.0 on a
    302 s measurement: encoder duty cycle 45.4% -> 18.2% at unchanged WER
    Pinned here so changing the literal has to come
    with a new measurement.

    The CLI is asserted against the same constants because it used to repeat
    the literals, and a default changed in one place would silently not change
    in the other. Needs no model and no fixtures, so it lives here rather than
    beside the model-backed streaming tests, which skip without both.
    """
    import argparse

    from myna.testbed.whisper import (
        STREAM_BEAM_SIZE,
        STREAM_CADENCE_S,
        STREAM_WINDOW_CAP_S,
        FasterWhisperAdapter,
    )

    assert (STREAM_CADENCE_S, STREAM_WINDOW_CAP_S, STREAM_BEAM_SIZE) == (2.0, 30.0, 1)

    adapter = FasterWhisperAdapter("tiny", streaming=True)
    assert adapter._stream_cadence_s == STREAM_CADENCE_S
    assert adapter._stream_window_cap_s == STREAM_WINDOW_CAP_S
    assert adapter._stream_beam_size == STREAM_BEAM_SIZE

    # No streaming flags on the namespace: the CLI must fall through to the
    # adapter's constants rather than to literals of its own.
    from_cli = build_adapter(
        argparse.Namespace(
            adapter="whisper", model="tiny", device="cpu", compute_type=None, streaming=True
        )
    )
    assert from_cli._stream_cadence_s == STREAM_CADENCE_S
    assert from_cli._stream_window_cap_s == STREAM_WINDOW_CAP_S
    assert from_cli._stream_beam_size == STREAM_BEAM_SIZE


def test_whisper_decode_options_are_the_measured_ones():
    """The temperature ladder is capped at two steps, on both paths.

    faster-whisper's default is six: a segment that trips the rejection test
    is re-decoded at every higher temperature in turn. Measured 2026-09-02 on
    the balanced tier, capping it leaves WER unchanged (tiny 6.21%, base 4.53%
    to four decimals; small 3.41% -> 3.38%) and cuts p95 decode latency 26% on
    tiny, 10% on base, and 25% on small.

    beam_size and condition_on_previous_text are asserted *absent* on the
    batch path: both were measured and both lose (beam 1 costs 0.50 pp for
    ~15%, the same trade T70 rejected for base int8; dropping the conditioning
    costs 0.20 pp for nothing). Pinned so a future edit has to bring a
    measurement, in either direction.
    """
    from myna.testbed.whisper import batch_decode_options, stream_decode_options

    batch = batch_decode_options("en-GB", None)
    assert batch["temperature"] == [0.0, 0.2]
    assert batch["log_prob_threshold"] == -0.5
    assert batch["language"] == "en"  # region subtag dropped for faster-whisper
    assert "beam_size" not in batch
    assert "condition_on_previous_text" not in batch

    # Same ladder on the streaming path. That one is a consistency and
    # robustness choice rather than a measured win: capping it moved the
    # streaming duty cycle 18.2% -> 18.1%, which is nothing.
    stream = stream_decode_options("en", None, beam_size=1)
    assert stream["temperature"] == batch["temperature"]
    assert stream["beam_size"] == 1
    assert stream["word_timestamps"] is True
    assert stream["vad_filter"] is False  # T71: costs accuracy on base


SNAP_ENV = {"SNAP_NAME": "myna-whisper", "SNAP_INSTANCE_NAME": "myna-whisper_dev"}


async def _serve_until_bound(args, monkeypatch):
    """Run serve() on the fake adapter, recording whether provider.env existed
    when the server was asked to bind."""
    import asyncio

    import myna.core
    from myna.server.cli import serve

    real_serve_unix = myna.core.serve_unix
    env_file_at_bind = []

    def recording_serve_unix(service, socket_path=None, *, sock=None):
        env_file_at_bind.append((args.socket.parent / "provider.env").exists())
        return real_serve_unix(service, socket_path, sock=sock)

    monkeypatch.setattr(myna.core, "serve_unix", recording_serve_unix)
    task = asyncio.ensure_future(serve(args))
    try:
        for _ in range(200):
            if args.socket.exists() or task.done():
                break
            await asyncio.sleep(0.01)
        assert args.socket.exists(), "server did not bind"
    finally:
        task.cancel()
        with contextlib.suppress(asyncio.CancelledError):
            await task
    return env_file_at_bind


async def test_share_provider_writes_provider_env_before_binding(tmp_path, monkeypatch):
    for key, value in SNAP_ENV.items():
        monkeypatch.setenv(key, value)
    socket = tmp_path / "share" / "provider" / "myna.sock"
    args = build_parser().parse_args(
        ["--socket", str(socket), "--adapter", "fake", "--share-provider"]
    )

    assert await _serve_until_bound(args, monkeypatch) == [True]
    assert (socket.parent / "provider.env").read_text() == (
        "SNAP_NAME=myna-whisper\nSNAP_INSTANCE_NAME=myna-whisper_dev\nUNIX_SOCKET=myna.sock\n"
    )


async def test_without_share_provider_no_env_file_is_written(tmp_path, monkeypatch):
    for key, value in SNAP_ENV.items():
        monkeypatch.setenv(key, value)
    socket = tmp_path / "myna.sock"
    args = build_parser().parse_args(["--socket", str(socket), "--adapter", "fake"])

    assert await _serve_until_bound(args, monkeypatch) == [False]
    assert not (tmp_path / "provider.env").exists()


def test_share_provider_without_snap_identity_exits_before_binding(tmp_path, monkeypatch):
    monkeypatch.setenv("SNAP_NAME", "myna-whisper")
    monkeypatch.delenv("SNAP_INSTANCE_NAME", raising=False)
    socket = tmp_path / "myna.sock"

    with pytest.raises(SystemExit, match="SNAP_INSTANCE_NAME"):
        main(["--socket", str(socket), "--adapter", "fake", "--share-provider"])

    assert list(tmp_path.iterdir()) == []
