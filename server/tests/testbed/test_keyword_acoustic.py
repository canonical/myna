"""CTC keyword correction over hand-built log-probabilities."""

from __future__ import annotations

import itertools

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.testbed.keyword_acoustic import (
    CtcKeywords,
    CtcToken,
    ctc_log_likelihood,
    greedy,
    tdt_log_likelihood,
)

PIECES = ["<blank>", "生", "耕", "申", "根", "签", "证", "，", "센", "서", "울", " ok"]
ID = {p: i for i, p in enumerate(PIECES)}


def encode(text):
    ids = []
    for ch in text:
        if ch not in ID:
            return None
        ids.append(ID[ch])
    return tuple(ids)


def frames(*columns):
    """Log-probabilities from per-frame {piece: probability} dicts; the rest
    of each frame's mass goes to blank."""
    lp = np.full((len(columns), len(PIECES)), 1e-6)
    for t, col in enumerate(columns):
        for piece, p in col.items():
            lp[t, ID[piece]] = p
        lp[t, 0] = max(1e-6, 1 - sum(col.values()))
    lp /= lp.sum(1, keepdims=True)
    return np.log(lp).astype(np.float32)


def tokens_of(lp):
    return [CtcToken(i, PIECES[i], first, last) for i, first, last, _ in greedy(lp, 0)]


def test_greedy_collapses_repeats_and_drops_blanks():
    lp = frames({"生": 0.9}, {"生": 0.9}, {}, {"耕": 0.8})
    assert [(i, a, b) for i, a, b, _ in greedy(lp, 0)] == [(1, 0, 1), (2, 3, 3)]


def test_log_likelihood_sums_every_alignment():
    lp = frames({"生": 0.6}, {"生": 0.3}, {"耕": 0.5})
    p = np.exp(lp.astype(np.float64))
    total = 0.0
    for path in itertools.product(range(len(PIECES)), repeat=3):
        collapsed = [k for k, _ in itertools.groupby(path) if k != 0]
        if collapsed == [ID["生"]]:
            total += np.prod([p[t, s] for t, s in enumerate(path)])
    assert ctc_log_likelihood(lp, 0, 2, [ID["生"]]) == pytest.approx(np.log(total))


def test_too_many_labels_for_the_frames_is_impossible():
    assert ctc_log_likelihood(frames({"生": 0.9}), 0, 0, [1, 2]) == -np.inf


def _homophone(keyword_share):
    """签证 heard as 生耕签证 where 申根 was said: the keyword's tokens carry
    ``keyword_share`` of the probability the greedy tokens have."""
    s = keyword_share
    return frames(
        {"生": 0.6, "申": 0.6 * s},
        {},
        {"耕": 0.6, "根": 0.6 * s},
        {},
        {"签": 0.9},
        {"证": 0.9},
    )


def test_a_keyword_the_audio_nearly_supports_replaces_the_run():
    lp = _homophone(0.5)
    assert CtcKeywords(["申根"], encode).corrections(tokens_of(lp), lp) == [(0, 2, "申根")]


def test_a_keyword_the_audio_does_not_support_is_left_out():
    lp = _homophone(1e-4)
    assert CtcKeywords(["申根"], encode).corrections(tokens_of(lp), lp) == []


def test_the_margin_scales_with_the_keyword_length():
    lp = _homophone(0.05)  # about 3 nats short per token
    assert CtcKeywords(["申根"], encode, margin=1.0).corrections(tokens_of(lp), lp) == []
    assert CtcKeywords(["申根"], encode, margin=4.0).corrections(tokens_of(lp), lp) == [
        (0, 2, "申根")
    ]


def test_a_run_already_spelling_the_keyword_is_left_alone():
    lp = frames({"申": 0.9}, {"根": 0.9})
    assert CtcKeywords(["申根"], encode).corrections(tokens_of(lp), lp) == []


def test_a_run_never_crosses_punctuation():
    lp = frames({"生": 0.6, "申": 0.5}, {"，": 0.9}, {"耕": 0.6, "根": 0.5})
    assert CtcKeywords(["申根"], encode).corrections(tokens_of(lp), lp) == []


def test_a_hangul_syllable_outside_the_vocabulary_scores_as_its_neighbour():
    # 셴 is not in the vocabulary; 센 (one vowel away) is.
    lp = frames({"서": 0.6, "센": 0.5}, {}, {"울": 0.9})
    keywords = CtcKeywords(["셴울"], encode)
    assert keywords
    assert keywords.corrections(tokens_of(lp), lp) == [(0, 2, "셴울")]


def test_latin_and_one_letter_keywords_are_not_scored():
    keywords = CtcKeywords(["Kaliko", "申", "ok 申根"], encode)
    assert keywords.keywords == ()
    assert not keywords


def test_tdt_likelihood_sums_label_and_blank_paths_over_durations():
    # One frame, two labels (blank 0, token 1), durations {0, 1}: emit the
    # token with duration 0 or 1; after a duration-0 emission the blank (or a
    # second emission) must still leave the frame.
    p_tok = np.array([0.25, 0.75])
    p_dur = np.array([0.4, 0.6])
    logits = np.log(np.concatenate([p_tok, p_dur]))[None, None, :].repeat(2, axis=1)
    expected = p_tok[1] * p_dur[1] + p_tok[1] * p_dur[0] * p_tok[0] * p_dur[1]
    assert tdt_log_likelihood(logits.astype(np.float32), [1], 2, 0) == pytest.approx(
        np.log(expected)
    )


# Latin keywords: whole words, spelt by ``encode_word`` with its word-start mark.
WORD_PIECES = ["<blank>", " re", "turned", " Ka", "li", "co", "ko", ".", " cat"]
WID = {p: i for i, p in enumerate(WORD_PIECES)}


def encode_word(text):
    table = {"Kaliko": (WID[" Ka"], WID["li"], WID["ko"])}
    return table.get(text)


def word_frames(*columns):
    lp = np.full((len(columns), len(WORD_PIECES)), 1e-15)
    for t, col in enumerate(columns):
        for piece, p in col.items():
            lp[t, WID[piece]] = p
        lp[t, 0] = max(1e-15, 1 - sum(col.values()))
    lp /= lp.sum(1, keepdims=True)
    return np.log(lp).astype(np.float32)


def word_tokens(lp):
    return [CtcToken(i, WORD_PIECES[i], a, b) for i, a, b, _ in greedy(lp, 0)]


def _calico(ko_share):
    return word_frames(
        {" re": 0.9},
        {"turned": 0.9},
        {" Ka": 0.9},
        {"li": 0.9},
        {"co": 0.6, "ko": 0.6 * ko_share},
        {".": 0.9},
    )


def test_a_latin_near_miss_the_audio_nearly_supports_becomes_the_keyword():
    lp = _calico(0.5)
    keywords = CtcKeywords(["Kaliko"], encode, encode_word=encode_word)
    assert keywords
    assert keywords.corrections(word_tokens(lp), lp) == [(2, 5, "Kaliko")]


def test_a_latin_near_miss_the_audio_does_not_support_is_kept():
    lp = _calico(1e-15)
    keywords = CtcKeywords(["Kaliko"], encode, encode_word=encode_word)
    assert keywords.corrections(word_tokens(lp), lp) == []


def test_a_latin_word_far_from_the_keyword_is_never_scored():
    lp = word_frames({" cat": 0.9, " Ka": 0.8}, {"li": 0.5}, {"ko": 0.5})
    keywords = CtcKeywords(["Kaliko"], encode, encode_word=encode_word)
    assert keywords.corrections(word_tokens(lp), lp) == []
