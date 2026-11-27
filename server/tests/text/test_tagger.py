"""The disfluency tagger around a stubbed session: decoding, windowing, loading."""

from __future__ import annotations

from types import SimpleNamespace

import numpy as np
import pytest

from myna.text import tagger
from myna.text.tagger import CAP, DELETE, KEEP, STRIP, Tagger, TaggerModel, decode, pick

SUBWORDS = 2


class FakeTokenizer:
    """Words become ids by number ("w7" -> 8) and are split in SUBWORDS pieces."""

    def encode(self, words, is_pretokenized):
        assert is_pretokenized
        ids, word_ids = [0], [None]
        for index, word in enumerate(words):
            for _ in range(SUBWORDS):
                ids.append((int(word[1:]) if word[1:].isdigit() else 0) + 1)
                word_ids.append(index)
        ids.append(0)
        word_ids.append(None)
        return SimpleNamespace(ids=ids, word_ids=word_ids)


class FakeSession:
    """Scores the tag chosen for each word number, remembering what it saw."""

    def __init__(self, plan: dict[int, int], confidence: float = 9.0):
        self.plan = plan
        self.confidence = confidence
        self.lengths: list[int] = []

    def run(self, outputs, feeds):
        ids = feeds["input_ids"][0]
        assert feeds["attention_mask"].shape == feeds["input_ids"].shape
        assert feeds["input_ids"].dtype == np.int64
        self.lengths.append((len(ids) - 2) // SUBWORDS)
        logits = np.zeros((1, len(ids), 4), dtype=np.float32)
        for position, ident in enumerate(ids):
            if 0 < position < len(ids) - 1:
                logits[0, position, self.plan.get(int(ident) - 1, KEEP)] = self.confidence
        return [logits]


def words(n: int) -> list[str]:
    return [f"w{i}" for i in range(n)]


def test_decode_applies_each_tag():
    toks = ["so,", "um,", "we", "go", "home,", "Bob"]
    tags = [KEEP, DELETE, KEEP, KEEP, STRIP, CAP]

    assert decode(toks, tags) == "so, we go home Bob"
    assert decode(["a"], [CAP]) == "A"
    assert decode(["a,"], [KEEP]) == "a,"
    assert decode([], []) == ""


def test_decode_moves_a_deleted_sentence_end_to_the_last_kept_word():
    assert decode(["we", "go", "uh."], [KEEP, KEEP, DELETE]) == "we go."
    assert decode(["we", "go,", "uh!"], [KEEP, KEEP, DELETE]) == "we go!"
    # Already ended: the mark is not doubled.
    assert decode(['"we', 'go."', "uh."], [KEEP, KEEP, DELETE]) == '"we go."'


def test_decode_capitalises_what_now_opens_a_sentence():
    assert decode(["um", "we", "go."], [DELETE, KEEP, KEEP]) == "We go."
    assert decode(["Hi.", "uh", "we", "go."], [KEEP, DELETE, KEEP, KEEP]) == "Hi. We go."
    # Not a word with capitals inside it, and not text that was never opened.
    assert decode(["um", "iPhone", "ok."], [DELETE, KEEP, KEEP]) == "iPhone ok."
    assert decode(["we", "uh", "go."], [KEEP, DELETE, KEEP]) == "we go."


def test_decode_capitalises_after_an_opening_mark():
    assert decode(["um", "¿puedes", "venir?"], [DELETE, KEEP, KEEP]) == "¿Puedes venir?"
    assert decode(["eh,", "«bonjour»", "a."], [DELETE, KEEP, KEEP]) == "«Bonjour» a."
    assert decode(["¿puedes", "venir?"], [CAP, KEEP]) == "¿Puedes venir?"
    assert decode(["um", "¿Puedes", "venir?"], [DELETE, KEEP, KEEP]) == "¿Puedes venir?"
    assert decode(["um", "¿", "venir?"], [DELETE, KEEP, KEEP]) == "¿ venir?"


def test_pick_demands_tau_for_a_delete_and_falls_back_to_the_next_best():
    logits = np.array([[0.0, 2.0, 1.0, 0.0], [0.0, 9.0, 0.0, 0.0], [5.0, 0.0, 0.0, 0.0]])

    assert pick(logits, 0.0) == [DELETE, DELETE, KEEP]
    assert pick(logits, 0.9) == [STRIP, DELETE, KEEP]


def test_a_short_text_is_tagged_in_one_pass_from_each_words_first_subword():
    session = FakeSession({1: DELETE, 3: CAP})
    t = Tagger(session, FakeTokenizer(), tau=0.9)

    assert t.tag(words(5)) == [KEEP, DELETE, KEEP, CAP, KEEP]
    assert session.lengths == [5]


def test_the_edge_of_single_pass_is_eighty_words():
    session = FakeSession({})
    t = Tagger(session, FakeTokenizer(), tau=0.9)

    t.tag(words(tagger.SINGLE))
    assert session.lengths == [tagger.SINGLE]
    session.lengths.clear()
    t.tag(words(tagger.SINGLE + 1))
    assert len(session.lengths) > 1


def test_a_long_text_is_tagged_in_overlapping_windows_covering_every_word():
    plan = {0: DELETE, 63: DELETE, 64: DELETE, 100: CAP, 199: DELETE}
    session = FakeSession(plan)
    t = Tagger(session, FakeTokenizer(), tau=0.9)

    tags = t.tag(words(200))

    assert tags == [plan.get(i, KEEP) for i in range(200)]
    assert max(session.lengths) == tagger.WINDOW
    assert session.lengths[:3] == [64, 64, 64]
    assert sum(session.lengths) >= 200


def test_each_word_takes_the_tag_of_the_window_that_sees_it_most_centrally():
    class Disagree(FakeSession):
        """Windows starting before 40 call word 60 KEEP, later windows call it DELETE."""

        def run(self, outputs, feeds):
            logits = super().run(outputs, feeds)[0]
            ids = feeds["input_ids"][0]
            for position, ident in enumerate(ids):
                if ident - 1 == 60 and 0 < position < len(ids) - 1:
                    first_word = int(ids[1]) - 1
                    logits[0, position] = 0.0
                    logits[0, position, DELETE if first_word >= 40 else KEEP] = 9.0
            return [logits]

    t = Tagger(Disagree({}), FakeTokenizer(), tau=0.5)

    # Word 60 is 28.5 from the middle of window 0 and 11.5 from that of window 40.
    assert t.tag(words(200))[60] == DELETE


def test_edit_returns_the_text_untouched_when_there_is_nothing_to_tag():
    t = Tagger(FakeSession({}), FakeTokenizer(), tau=0.9)

    assert t.edit("   ") == "   "
    assert t.edit("") == ""


def test_edit_deletes_tagged_words_and_rejoins_on_single_spaces():
    t = Tagger(FakeSession({1: DELETE}), FakeTokenizer(), tau=0.9)

    assert t.edit("w0   w1\tw2") == "w0 w2"


def manifest(**extra):
    return {
        "id": "m",
        "task": "text.cleanup",
        "language": "en",
        "latency_ms": 10,
        "quality": 0.9,
        "tau": 0.8,
        **extra,
    }


def test_a_manifest_listing_languages_covers_each_of_them(tmp_path):
    model = TaggerModel(tmp_path, manifest(languages=["en", "fr", "de", "es"]))
    assert model.capability()["covers"] == {"language": ["en", "fr", "de", "es"]}
    assert TaggerModel(tmp_path, manifest()).capability()["covers"] == {"language": ["en"]}


def test_the_model_loads_once_on_first_use_and_again_after_close(monkeypatch, tmp_path):
    loads = []

    def load(directory, tau):
        loads.append((directory, tau))
        return Tagger(FakeSession({}), FakeTokenizer(), tau)

    monkeypatch.setattr(Tagger, "load", staticmethod(load))
    model = TaggerModel(tmp_path, manifest())

    assert model.capability() == {
        "id": "m",
        "task": "text.cleanup",
        "covers": {"language": ["en"]},
        "accepts": [],
        "class": "edit",
        "latency_ms": 10,
        "quality": 0.9,
    }
    assert loads == []
    assert model.run("a b", 1.0) == "a b"
    model.warm(1.0)
    assert loads == [(tmp_path, 0.8)]
    model.close()
    assert model.run("a b", 1.0) == "a b"
    assert len(loads) == 2


def test_decode_ends_a_quoted_sentence_before_the_next_opens():
    assert decode(['"Go."', "um", "we", "stay."], [KEEP, DELETE, KEEP, KEEP]) == '"Go." We stay.'


def test_only_what_follows_a_deletion_at_a_sentence_start_is_capitalised():
    toks = ["um", "we", "go.", "so", "ok."]

    assert decode(toks, [DELETE, KEEP, KEEP, KEEP, KEEP]) == "We go. so ok."


def test_the_stop_of_a_deleted_last_word_lands_on_the_last_kept_word():
    assert decode(["we", "go", "home", "uh."], [KEEP, KEEP, KEEP, DELETE]) == "we go home."
    assert decode(["we", "ok.", "uh."], [KEEP, KEEP, DELETE]) == "we ok."
    assert decode(["we", "ok;", "uh?"], [KEEP, KEEP, DELETE]) == "we ok?"


def test_decode_refuses_tags_that_do_not_match_the_words():
    with pytest.raises(ValueError, match="shorter"):
        decode(["a", "b"], [KEEP])


def test_a_single_word_and_no_words_at_all():
    session = FakeSession({0: DELETE})
    t = Tagger(session, FakeTokenizer(), tau=0.9)

    assert t.tag([]) == []
    assert session.lengths == []
    assert t.tag(["w0"]) == [DELETE]


def test_every_word_of_a_long_text_takes_the_window_that_centres_it_best():
    class WindowStart(FakeSession):
        """Tags every word of a window with the number of that window."""

        def run(self, outputs, feeds):
            ids = feeds["input_ids"][0]
            start = int(ids[1]) - 1
            logits = np.zeros((1, len(ids), 4), dtype=np.float32)
            logits[0, 1:-1, start // tagger.STRIDE] = 9.0
            self.lengths.append((len(ids) - 2) // SUBWORDS)
            return [logits]

    count = 150
    t = Tagger(WindowStart({}), FakeTokenizer(), tau=0.0)
    starts = [0, 40, 80, 120]

    def centre(start):
        return (start + min(count, start + tagger.WINDOW) - 1) / 2

    expected = [
        min((s for s in starts if s <= i < s + tagger.WINDOW), key=lambda s: abs(i - centre(s)))
        // tagger.STRIDE
        for i in range(count)
    ]
    assert t.tag(words(count)) == expected


def test_a_model_loads_from_its_own_directory(monkeypatch, tmp_path):
    seen = []
    monkeypatch.setattr(
        Tagger,
        "load",
        staticmethod(
            lambda d, tau: seen.append(d) or Tagger(FakeSession({}), FakeTokenizer(), tau)
        ),
    )

    TaggerModel(tmp_path, manifest()).warm(1.0)

    assert seen == [tmp_path]


def test_a_deleted_word_ending_in_a_quote_still_passes_its_stop_on():
    assert decode(["we", "go", 'uh."'], [KEEP, KEEP, DELETE]) == "we go."


def test_a_one_letter_word_that_opens_a_sentence_is_capitalised():
    assert decode(["um", "a", "dog."], [DELETE, KEEP, KEEP]) == "A dog."


def test_the_stop_goes_after_a_last_word_that_ends_in_a_capital_x():
    assert decode(["the", "FAX", "uh."], [KEEP, KEEP, DELETE]) == "the FAX."
