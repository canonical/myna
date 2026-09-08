"""Folding submissions from many machines into one tracked file.

The whole point of collecting results is comparing machines, so a row's
identity is (machine, label) - not label. Keyed on label alone, merging two
submissions kept one and dropped the other with no warning, which is the
failure these tests exist to prevent.

`merge` is deliberately boring: replace each submitting machine's rows, leave
every other machine alone, refuse the two mistakes that are silent otherwise.
"""

from __future__ import annotations

import json

import pytest
from _records import record

from myna.benchmarker._summarize import (
    _load_latest,
    _summarize,
    cmd_merge,
    machine_of,
    one_machine_per_name,
    row_key,
)


def write_jsonl(path, records) -> None:
    path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")


def read_jsonl(path) -> list[dict]:
    return [json.loads(ln) for ln in path.read_text(encoding="utf-8").splitlines() if ln.strip()]


def submission(machine, *, label="myna-whisper/cpu/tiny/batch", clips=("c1",), **kw):
    cpu = kw.pop("cpu", f"{machine}-cpu")
    return [
        record(label=label, clip=clip, provenance={"machine": machine, "cpu": cpu}, **kw)
        for clip in clips
    ]


class MergeArgs:
    def __init__(self, leaderboard, *results, allow_mixed_corpora=False):
        self.leaderboard = str(leaderboard)
        self.results = [str(r) for r in results]
        self.allow_mixed_corpora = allow_mixed_corpora


# ─── row identity ────────────────────────────────────────────────────────────


def test_the_same_label_on_two_machines_is_two_rows(tmp_path):
    """The regression that motivated the key: one submission used to disappear."""
    path = tmp_path / "leaderboard.jsonl"
    write_jsonl(path, submission("framework") + submission("thinkpad"))

    records, _ = _load_latest(path)
    summary = _summarize(records)

    assert len(records) == 2
    assert sorted(summary) == [
        ("framework", "myna-whisper/cpu/tiny/batch"),
        ("thinkpad", "myna-whisper/cpu/tiny/batch"),
    ]


def test_a_rerun_on_one_machine_still_replaces_its_own_row(tmp_path):
    path = tmp_path / "leaderboard.jsonl"
    write_jsonl(path, submission("framework", wer_edits=5) + submission("framework", wer_edits=1))
    records, _ = _load_latest(path)
    assert len(records) == 1
    assert records[0]["wer_edits"] == 1


def test_a_status_record_binds_to_its_own_machine(tmp_path):
    """Keyed on the label alone, one host's failure would flag every host."""
    path = tmp_path / "leaderboard.jsonl"
    write_jsonl(
        path,
        submission("framework")
        + submission("thinkpad")
        + [
            {
                "machine": "thinkpad",
                "label": "myna-whisper/cpu/tiny/batch",
                "status": "usability_fail",
                "reason": "slow",
            }
        ],
    )
    _, statuses = _load_latest(path)
    assert statuses == {("thinkpad", "myna-whisper/cpu/tiny/batch"): ("usability_fail", "slow")}


def test_the_machine_comes_from_provenance_then_the_bare_field():
    assert machine_of(record(provenance={"machine": "framework"})) == "framework"
    assert machine_of({"machine": "box", "label": "x"}) == "box"
    assert machine_of(record()) == "unknown"
    assert row_key(record(label="lbl")) == ("unknown", "lbl")


def test_two_hosts_sharing_a_hostname_are_refused_not_averaged():
    rows = submission("framework", cpu="Ryzen AI 7 350") + submission("framework", cpu="i7-1185G7")
    with pytest.raises(SystemExit, match="more than one CPU"):
        one_machine_per_name(rows)


def test_one_host_with_a_consistent_cpu_passes():
    one_machine_per_name(submission("framework", clips=("c1", "c2")))


# ─── merge ───────────────────────────────────────────────────────────────────


def test_merging_into_an_empty_leaderboard_creates_it(tmp_path, capsys):
    board = tmp_path / "leaderboard.jsonl"
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("framework"))

    cmd_merge(MergeArgs(board, incoming))

    assert len(read_jsonl(board)) == 1
    assert "merged 1 record(s) from framework" in capsys.readouterr().out


def test_a_second_machine_is_added_beside_the_first(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    write_jsonl(board, submission("framework"))
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("thinkpad"))

    cmd_merge(MergeArgs(board, incoming))

    assert {machine_of(r) for r in read_jsonl(board)} == {"framework", "thinkpad"}


def test_resubmitting_replaces_that_machines_rows_only(tmp_path, capsys):
    board = tmp_path / "leaderboard.jsonl"
    write_jsonl(
        board, submission("framework", clips=("c1", "c2"), wer_edits=9) + submission("thinkpad")
    )
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("framework", wer_edits=1))

    cmd_merge(MergeArgs(board, incoming))

    rows = read_jsonl(board)
    framework = [r for r in rows if machine_of(r) == "framework"]
    assert len(framework) == 1 and framework[0]["wer_edits"] == 1
    assert len([r for r in rows if machine_of(r) == "thinkpad"]) == 1
    assert "replaced 2 earlier record(s)" in capsys.readouterr().out


def test_several_submissions_merge_in_one_call(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    a, b = tmp_path / "a.jsonl", tmp_path / "b.jsonl"
    write_jsonl(a, submission("framework"))
    write_jsonl(b, submission("thinkpad"))

    cmd_merge(MergeArgs(board, a, b))

    assert {machine_of(r) for r in read_jsonl(board)} == {"framework", "thinkpad"}


def test_a_submission_against_another_corpus_is_refused(tmp_path):
    """Ranking across two corpora ranks nothing: different audio, different
    reference text."""
    board = tmp_path / "leaderboard.jsonl"
    write_jsonl(board, submission("framework"))
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("thinkpad", corpus_id="v1:other"))

    with pytest.raises(SystemExit, match="more than one corpus"):
        cmd_merge(MergeArgs(board, incoming))

    assert len(read_jsonl(board)) == 1  # left untouched


def test_mixed_corpora_can_be_forced_for_a_deliberate_split(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    write_jsonl(board, submission("framework"))
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("thinkpad", corpus_id="v1:other"))

    cmd_merge(MergeArgs(board, incoming, allow_mixed_corpora=True))

    assert len(read_jsonl(board)) == 2


def test_a_hostname_clash_is_refused_before_the_board_is_written(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    write_jsonl(board, submission("framework", cpu="Ryzen AI 7 350"))
    incoming = tmp_path / "results.jsonl"
    write_jsonl(incoming, submission("framework", cpu="i7-1185G7", clips=("c2",)))

    with pytest.raises(SystemExit, match="more than one CPU"):
        cmd_merge(MergeArgs(board, incoming))


def test_status_rows_ride_along_with_their_clips(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    incoming = tmp_path / "results.jsonl"
    write_jsonl(
        incoming,
        [
            {"type": "machine", "hostname": "framework"},
            *submission("framework"),
            {
                "machine": "framework",
                "label": "myna-whisper/cpu/tiny/batch",
                "status": "ok",
                "reason": "",
            },
        ],
    )

    cmd_merge(MergeArgs(board, incoming))

    rows = read_jsonl(board)
    assert not any(r.get("type") == "machine" for r in rows)  # header is per-file, not per-row
    assert any("status" in r for r in rows)


def test_merging_nothing_is_an_error(tmp_path):
    board = tmp_path / "leaderboard.jsonl"
    empty = tmp_path / "empty.jsonl"
    empty.write_text("", encoding="utf-8")
    with pytest.raises(SystemExit, match="nothing to merge"):
        cmd_merge(MergeArgs(board, empty))


def test_a_missing_submission_names_the_path(tmp_path):
    with pytest.raises(SystemExit, match="no results at"):
        cmd_merge(MergeArgs(tmp_path / "leaderboard.jsonl", tmp_path / "absent.jsonl"))
