"""Tests for the server CLI's streaming-flag validation and adapter dispatch.

The re-decode tuning flags are whisper-only (contracts/strategy-config.md) and
misuse only warns, so the log is the whole signal - assert on it.
"""

import logging

import pytest

from myna.server.cli import _validate_streaming_args, build_adapter, build_parser


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
