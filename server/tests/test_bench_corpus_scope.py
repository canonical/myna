"""A comparison table must be for one corpus (dev/aggregate.py).

Micro-averaging WER across two corpora compares nothing: the rows were scored
against different audio and different reference text. The runner stamps every
record with the corpus id it measured, and the aggregator refuses anything else.
"""

from __future__ import annotations

import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "dev"))

import aggregate  # noqa: E402


def rec(label: str, corpus: str | None) -> dict:
    row = {"label": label, "clip": "c1"}
    if corpus is not None:
        row["corpus_id"] = corpus
    return row


def test_one_corpus_passes_through():
    records = [rec("a", "v1:aaaa"), rec("b", "v1:aaaa")]
    kept, corpus = aggregate.one_corpus(records, None)
    assert corpus == "v1:aaaa"
    assert kept == records


def test_two_corpora_abort():
    with pytest.raises(SystemExit, match="compares nothing"):
        aggregate.one_corpus([rec("a", "v1:aaaa"), rec("b", "v1:bbbb")], None)


def test_two_corpora_can_be_narrowed_explicitly():
    kept, corpus = aggregate.one_corpus([rec("a", "v1:aaaa"), rec("b", "v1:bbbb")], "v1:bbbb")
    assert corpus == "v1:bbbb"
    assert [r["label"] for r in kept] == ["b"]


def test_an_unstamped_record_aborts():
    """Nothing says what audio produced it, so it cannot join a table."""
    with pytest.raises(SystemExit, match="no corpus_id"):
        aggregate.one_corpus([rec("a", "v1:aaaa"), rec("legacy", None)], None)
