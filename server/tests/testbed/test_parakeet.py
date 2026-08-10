"""Parakeet adapter units (008 US3) — model-free helpers only.

The decode port itself is exercised end-to-end by dev/bench.py against the
staged int8 weights (671 MB — not a unit-test fixture); here we pin the pure
text/vocab mechanics the emission loop depends on (I2 verbatim concat).
"""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.server.cli import build_adapter, build_parser
from myna.testbed.parakeet import (
    ParakeetAdapter,
    _detokenize,
    _load_vocab,
    _tokens_to_words,
)
from myna.testbed.streaming.strategies import SilenceCut


def test_detokenize_strips_leading_and_pre_punctuation_spaces():
    # ▁→space already applied at vocab load; murmure's DECODE_SPACE_RE parity.
    tokens = [" Hello", ",", " world", "!", " It", " is", " me", "."]
    assert _detokenize(tokens) == "Hello, world! It is me."


def test_detokenize_empty():
    assert _detokenize([]) == ""


def test_tokens_to_words_groups_subwords_with_natural_spacing():
    tokens = [" Hel", "lo", " world", "!"]
    timestamps = [0.0, 0.08, 0.16, 0.32]
    words = _tokens_to_words(tokens, timestamps)
    # Punctuation attaches to its word (whisper word-text parity: " world!").
    assert [w.text for w in words] == [" Hello", " world!"]
    # Word spans run token-start to next-token-start; the last gets one frame.
    assert words[0].start == 0.0 and words[0].end == 0.16
    assert words[-1].end == 0.32 + 0.08


def test_tokens_to_words_first_token_without_space_still_opens_a_word():
    words = _tokens_to_words(["Hel", "lo", " again"], [0.0, 0.08, 0.16])
    assert [w.text for w in words] == ["Hello", " again"]


def test_load_vocab(tmp_path):
    vocab_file = tmp_path / "vocab.txt"
    vocab_file.write_text("<unk> 0\n▁hello 5\nworld 9\n<blk> 10\n", encoding="utf-8")
    vocab, blank = _load_vocab(str(tmp_path))
    assert blank == 10
    assert vocab[5] == " hello"  # ▁ becomes a literal space
    assert vocab[9] == "world"
    assert len(vocab) == 11


async def test_streaming_cut_constants_are_configurable(monkeypatch):
    """The snap exposes the SilenceCut knobs; the adapter must wire them into
    the strategy instead of always using murmure's defaults."""
    adapter = ParakeetAdapter(
        streaming=True,
        stream_arm_s=2.5,
        stream_silence_cut_s=0.25,
        stream_force_cut_s=7.0,
    )
    captured = {}

    async def fake_loop(audio, emit, decode, strategy, **kwargs):
        captured["strategy"] = strategy
        captured["kwargs"] = kwargs
        return ""

    monkeypatch.setattr("myna.testbed.streaming.loop.run_streaming_loop", fake_loop)

    async def empty_audio():
        for _ in ():
            yield b""  # pragma: no cover - async iterator shape only

    async def emit(_event):
        pass

    await adapter._run_streaming_session(object(), empty_audio(), emit)

    strategy = captured["strategy"]
    assert isinstance(strategy, SilenceCut)
    assert strategy._arm == 2.5
    assert strategy._silence_cut == 0.25
    assert strategy._force_cut == 7.0
    assert captured["kwargs"]["window_cap_seconds"] == 12.0


def test_streaming_cut_constants_must_be_positive():
    with pytest.raises(ValueError, match="stream_arm_s"):
        ParakeetAdapter(stream_arm_s=0)
    with pytest.raises(ValueError, match="stream_silence_cut_s"):
        ParakeetAdapter(stream_silence_cut_s=0)
    with pytest.raises(ValueError, match="stream_force_cut_s"):
        ParakeetAdapter(stream_force_cut_s=0)


def test_cli_wires_streaming_cut_constants():
    args = build_parser().parse_args(
        [
            "--socket",
            "/tmp/s.sock",
            "--adapter",
            "parakeet",
            "--streaming",
            "--stream-arm-s",
            "2.5",
            "--stream-silence-cut-s",
            "0.25",
            "--stream-force-cut-s",
            "7.0",
        ]
    )
    adapter = build_adapter(args)
    assert adapter._stream_arm_s == 2.5
    assert adapter._stream_silence_cut_s == 0.25
    assert adapter._stream_force_cut_s == 7.0
