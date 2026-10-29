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


def _candidates(bias, state):
    """Token ids that extend a match from ``state``."""
    return bias._offer(state)[0]


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
    assert ID["ob"] not in _candidates(bias, ROOT)
    assert ID[" Si"] in _candidates(bias, ROOT)


def test_a_lowercase_keyword_also_matches_capitalised():
    bias = _bias("siobhan")
    assert bias.complete(_walk(bias, " si", "ob", "han"))
    assert bias.complete(_walk(bias, " Si", "ob", "han"))


def test_a_capitalised_keyword_does_not_match_lowercase():
    bias = _bias("Siobhan")
    assert ID[" si"] not in _candidates(bias, ROOT)


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
    offered = set(_candidates(bias, _walk(bias, " New", " Y")).tolist())
    assert {ID["ak"], ID["am"], ID[" Si"], ID[" New"]} <= offered
    assert ID["ob"] not in offered


def test_a_completed_keyword_offers_new_word_starts_only():
    bias = _bias("Siobhan")
    offered = set(_candidates(bias, _walk(bias, " Si", "ob", "han")).tolist())
    assert offered == set(_candidates(bias, ROOT).tolist())


def test_blank_is_never_a_candidate():
    bias = _bias("Siobhan", "New York")
    state = _walk(bias, " Si")
    assert BLANK not in _candidates(bias, ROOT)
    assert BLANK not in _candidates(bias, state)


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


def test_a_rival_beats_the_argmax_unboosted_or_leaves_it():
    bias = _bias("Siobhan", boost=1.0)
    close = _logits(**{" Sio": 9.0, " Si": 8.0})
    assert bias.rerank(ROOT, close, ID[" Sio"]) == ID[" Sio"]
    assert bias.rerank(ROOT, close, ID[" Sio"], rival=True) == ID[" Si"]
    far = _logits(**{" Sio": 9.0, " Si": 6.0})
    assert bias.rerank(ROOT, far, ID[" Sio"], rival=True) == ID[" Sio"]


def _correct(keywords, *pieces):
    """Run ``correct`` over (token, confidence) pairs; tokens time 0.1 s apart."""
    tokens = [token for token, _ in pieces]
    confidences = [confidence for _, confidence in pieces]
    timestamps = [0.1 * i for i in range(len(tokens))]
    return _bias(*keywords).correct(tokens, timestamps, confidences)


def test_a_word_spelt_as_a_keyword_but_for_accents_and_case_takes_its_spelling():
    assert _correct(["Siobhan", "Sepúlveda"], (" the", 0.99), (" sepulveda", 0.99)) == (
        [" the", " Sepúlveda"],
        pytest.approx([0.0, 0.1]),
    )


def test_a_lowercase_keyword_keeps_the_capital_it_was_written_with():
    assert _correct(["Siobhan", "asparagus"], (" Asparagus", 0.99))[0] == [" Asparagus"]


def test_an_unsure_word_one_edit_from_a_keyword_becomes_it():
    assert _correct(["Siobhan"], (" Sio", 0.95), ("bhn", 0.5), (",", 0.99), (" the", 0.9)) == (
        [" Siobhan,", " the"],
        pytest.approx([0.0, 0.3]),
    )


def test_a_confident_word_is_never_corrected():
    tokens = [(" Sio", 0.95), ("bhn", 0.85)]
    assert _correct(["Siobhan"], *tokens)[0] == [" Sio", "bhn"]


def test_a_long_word_may_be_two_edits_away():
    assert _correct(["Siobhan", "Noirtier"], (" Noartyer", 0.5))[0] == [" Noirtier"]


def test_a_short_word_two_edits_away_is_left_alone():
    # "Minya" for "Myna", heard live 2026-10-13: two edits on five letters
    # reach too many ordinary words to correct without a false hit.
    assert _correct(["Siobhan", "Myna"], (" Minya", 0.1))[0] == [" Minya"]


def test_a_word_under_five_letters_is_never_fuzzy_corrected():
    assert _correct(["Siobhan", "Kivu"], (" Kibu", 0.1))[0] == [" Kibu"]


def test_a_word_equally_close_to_two_keywords_is_left_alone():
    assert _correct(["Siobhan", "Siobhen", "Siobhun"], (" Siobhin", 0.1))[0] == [" Siobhin"]


def test_a_phrase_keyword_is_not_a_correction_target():
    assert _correct(["Siobhan", "New York"], (" Yorke", 0.1))[0] == [" Yorke"]


def test_words_that_spell_a_keyword_together_become_it():
    assert _correct(
        ["Siobhan", "PulseAudio"], (" pulse", 0.99), (" audio", 0.99), (" is", 0.99)
    ) == (
        [" PulseAudio", " is"],
        pytest.approx([0.0, 0.2]),
    )


def test_a_joined_keyword_keeps_the_last_words_punctuation():
    pieces = [(" the", 0.99), (" Pulse", 0.99), (" Audio", 0.99), (".", 0.99)]
    assert _correct(["Siobhan", "PulseAudio"], *pieces)[0] == [" the", " PulseAudio."]


def test_three_words_may_spell_a_keyword():
    pieces = [(" test", 0.99), (" fling", 0.99), (" er", 0.99)]
    assert _correct(["Siobhan", "Testflinger"], *pieces)[0] == [" Testflinger"]


def test_punctuation_between_words_keeps_them_apart():
    pieces = [(" pulse", 0.99), (",", 0.99), (" audio", 0.99)]
    assert _correct(["Siobhan", "PulseAudio"], *pieces)[0] == [" pulse", ",", " audio"]


def test_words_spelling_more_than_a_keyword_are_left_alone():
    pieces = [(" pulse", 0.99), (" audios", 0.99)]
    assert _correct(["Siobhan", "PulseAudio"], *pieces)[0] == [" pulse", " audios"]


def test_a_word_with_no_letters_is_left_alone():
    assert _correct(["Siobhan"], (" 42", 0.1))[0] == [" 42"]
