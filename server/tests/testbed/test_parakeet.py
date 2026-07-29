"""Parakeet adapter units (008 US3) — model-free helpers only.

The decode port itself is exercised end-to-end by dev/bench.py against the
staged int8 weights (671 MB — not a unit-test fixture); here we pin the pure
text/vocab mechanics the emission loop depends on (I2 verbatim concat).
"""

from __future__ import annotations

from myna.testbed.parakeet import _detokenize, _load_vocab, _tokens_to_words


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
