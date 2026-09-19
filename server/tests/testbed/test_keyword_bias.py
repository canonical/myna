"""Keyword biasing matcher: tokenisation into the model's own subwords, the
match automaton, and the re-ranking rule, over a toy vocabulary."""

from __future__ import annotations

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.testbed.keyword_bias import ROOT, KeywordBias, keyword_bias

# As _load_vocab returns it: "▁" already a space, blank spelt as in vocab.txt.
VOCAB = [
    "<unk>",
    " ",
    " S",
    " Si",
    "o",
    "ob",
    "bhan",
    "han",
    " si",
    "h",
    "a",
    "n",
    " the",
    " New",
    " York",
    "b",
    " Y",
    "ak",
    "am",
    " Sio",
    "Si",
    "<|nospeech|>",
    "<blk>",
]
BLANK = len(VOCAB) - 1
ID = {token: i for i, token in enumerate(VOCAB)}


def _bias(*keywords: str, boost: float = 1.0) -> KeywordBias:
    bias = keyword_bias(keywords, VOCAB, BLANK, boost_per_char=boost)
    assert bias is not None
    return bias


def _walk(bias: KeywordBias, *tokens: str) -> int:
    state = ROOT
    for token in tokens:
        state, _ = bias.advance(state, ID[token])
    return state


def _logits(**scores: float) -> np.ndarray:
    logits = np.full(len(VOCAB), -50.0, dtype=np.float32)
    for token, score in scores.items():
        logits[ID[token]] = score
    return logits


def test_no_keywords_builds_nothing():
    assert keyword_bias((), VOCAB, BLANK) is None


def test_a_keyword_the_vocab_cannot_spell_builds_nothing():
    assert keyword_bias(("Zoë",), VOCAB, BLANK) is None


@pytest.mark.parametrize("keyword", ["<unk>", "<blk>", "<|nospeech|>"])
def test_special_tokens_never_spell_a_keyword(keyword):
    assert keyword_bias((keyword,), VOCAB, BLANK) is None


def test_every_segmentation_the_vocab_allows_is_a_path():
    bias = _bias("Siobhan")
    for path in ((" Si", "o", "bhan"), (" Si", "ob", "han"), (" Sio", "b", "h", "a", "n")):
        assert bias.complete(_walk(bias, *path)), path


def test_a_keyword_matches_only_at_a_word_start():
    bias = _bias("Siobhan")
    assert ID["ob"] not in bias.candidates(ROOT)
    assert ID[" Si"] in bias.candidates(ROOT)


def test_a_lowercase_keyword_also_matches_capitalised():
    bias = _bias("siobhan")
    assert bias.complete(_walk(bias, " si", "ob", "han"))
    assert bias.complete(_walk(bias, " Si", "ob", "han"))


def test_a_capitalised_keyword_does_not_match_lowercase():
    bias = _bias("Siobhan")
    assert ID[" si"] not in bias.candidates(ROOT)


def test_inner_whitespace_is_normalised():
    bias = _bias("  New   York ")
    assert bias.complete(_walk(bias, " New", " York"))


def test_a_prefix_is_not_complete():
    bias = _bias("Siobhan")
    state = _walk(bias, " Si", "ob")
    assert state != ROOT
    assert not bias.complete(state)
    assert not bias.complete(ROOT)


def test_extending_a_match_is_reported():
    bias = _bias("Siobhan")
    state, extended = bias.advance(_walk(bias, " Si"), ID["ob"])
    assert extended and state == _walk(bias, " Si", "ob")


def test_a_token_off_the_path_breaks_the_match_back_to_the_root():
    bias = _bias("Siobhan")
    assert bias.advance(_walk(bias, " Si", "ob"), ID[" the"]) == (ROOT, False)


def test_a_break_can_start_another_keyword():
    bias = _bias("New Yak", "York")
    state, extended = bias.advance(_walk(bias, " New"), ID[" York"])
    assert not extended
    assert bias.complete(state)


def test_a_bare_word_boundary_can_be_followed_by_a_fresh_start():
    bias = _bias("Siobhan")
    state, extended = bias.advance(_walk(bias, " "), ID[" Si"])
    assert not extended
    assert state == _walk(bias, " Si")


def test_a_break_follows_the_longest_suffix_still_matching():
    bias = _bias("New Yak", "Yam")
    state, extended = bias.advance(_walk(bias, " New", " Y"), ID["am"])
    assert not extended
    assert bias.complete(state)


def test_a_partial_match_offers_its_continuations_and_new_word_starts():
    bias = _bias("New Yak", "Yam", "Siobhan")
    offered = set(bias.candidates(_walk(bias, " New", " Y")).tolist())
    assert {ID["ak"], ID["am"], ID[" Si"], ID[" New"]} <= offered
    assert ID["ob"] not in offered


def test_a_completed_keyword_offers_new_word_starts_only():
    bias = _bias("Siobhan")
    offered = set(bias.candidates(_walk(bias, " Si", "ob", "han")).tolist())
    assert offered == set(bias.candidates(ROOT).tolist())


def test_blank_is_never_a_candidate():
    bias = _bias("Siobhan", "New York")
    state = _walk(bias, " Si")
    assert BLANK not in bias.candidates(ROOT)
    assert BLANK not in bias.candidates(state)


def test_rerank_prefers_a_candidate_within_its_bonus():
    bias = _bias("Siobhan", boost=3.0)
    # " Si" spells two keyword characters: 0 + 2 * 3 beats 5.
    assert bias.rerank(ROOT, _logits(**{" the": 5.0, " Si": 0.0}), ID[" the"]) == ID[" Si"]


def test_rerank_keeps_the_argmax_beyond_the_bonus():
    bias = _bias("Siobhan", boost=3.0)
    assert bias.rerank(ROOT, _logits(**{" the": 6.5, " Si": 0.0}), ID[" the"]) == ID[" the"]


def test_rerank_needs_a_strict_win():
    bias = _bias("Siobhan", boost=3.0)
    logits = _logits(**{" the": 6.0, " Si": 0.0})
    assert bias.rerank(ROOT, logits, ID[" the"]) == ID[" the"]


def test_the_bonus_grows_with_the_characters_a_token_spells():
    bias = _bias("Siobhan", boost=1.0)
    logits = _logits(**{" the": 2.5, " S": 1.0, " Si": 0.5, " Sio": -0.4})
    # " S" 1+1=2, " Si" 0.5+2=2.5 (a tie loses), " Sio" -0.4+3=2.6 wins.
    assert bias.rerank(ROOT, logits, ID[" the"]) == ID[" Sio"]


def test_a_bare_word_boundary_counts_as_one_character():
    bias = _bias("Siobhan", boost=1.0)
    assert bias.rerank(ROOT, _logits(**{" the": 0.5, " ": 0.0}), ID[" the"]) == ID[" "]
    assert bias.rerank(ROOT, _logits(**{" the": 1.5, " ": 0.0}), ID[" the"]) == ID[" the"]


def test_rerank_continues_a_partial_match():
    bias = _bias("Siobhan", boost=1.0)
    state = _walk(bias, " Si")
    logits = _logits(**{" the": 1.0, "ob": 0.0})
    assert bias.rerank(state, logits, ID[" the"]) == ID["ob"]


def test_the_argmax_itself_is_kept_when_it_is_the_best_candidate():
    bias = _bias("Siobhan", boost=1.0)
    logits = _logits(**{" Sio": 9.0})
    assert bias.rerank(ROOT, logits, ID[" Sio"]) == ID[" Sio"]
