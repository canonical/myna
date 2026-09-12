"""Aggregate table for `myna-bench summarize`.

The table is what testers actually submit conclusions from, so the arithmetic
gets pinned here: last-write-wins de-duplication, micro-averaged (not
per-clip-averaged) WER, warm/cold separation, and the None-tolerant formatting
that keeps a partially-failed sweep readable instead of crashing the report.
"""

from __future__ import annotations

import json

import pytest
from _records import record

from myna.benchmarker._summarize import (
    _f,
    _load_latest,
    _load_resources,
    _pct,
    _print_by_category,
    _print_overall,
    _speed,
    _summarize,
    cmd_summarize,
    one_corpus,
    ranked_labels,
)

UNKNOWN = ("unknown", "whisper/cpu/tiny/batch")


def write_jsonl(path, records) -> None:
    path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")


def print_overall(summary: dict) -> None:
    """Render with no status records - the common case for a plain results file."""
    _print_overall(summary, ranked_labels(summary, "label", {}), {})


def print_overall_of(summary: dict) -> None:
    print_overall(summary)


def print_by_category(records: list) -> None:
    summary = _summarize(records)
    _print_by_category(records, ranked_labels(summary, "label", {}), {})


# ─── _load_latest ────────────────────────────────────────────────────────────


def test_load_latest_keeps_the_last_record_per_label_clip_cold(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(transcript="first", wer_edits=2),
            record(transcript="second", wer_edits=1),
        ],
    )
    (loaded,), _ = _load_latest(path)
    assert loaded["transcript"] == "second"


def test_load_latest_separates_cold_from_warm_for_the_same_clip(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(cold=True), record(cold=False)])
    records, _ = _load_latest(path)
    assert {r["cold"] for r in records} == {True, False}


def test_load_latest_skips_the_machine_header_and_error_records(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            {"type": "machine", "hostname": "box"},
            record(clip="bad", error={"code": "adapter_failed", "message": "boom"}),
            record(clip="good"),
        ],
    )
    records, _ = _load_latest(path)
    assert [r["clip"] for r in records] == ["good"]


def test_load_latest_tolerates_blank_lines(tmp_path):
    path = tmp_path / "results.jsonl"
    path.write_text(json.dumps(record()) + "\n\n   \n", encoding="utf-8")
    assert len(_load_latest(path)[0]) == 1


def test_load_latest_on_a_missing_file_exits_with_the_path(tmp_path):
    with pytest.raises(SystemExit, match="no results at"):
        _load_latest(tmp_path / "absent.jsonl")


# ─── _pct ────────────────────────────────────────────────────────────────────


def test_pct_of_nothing_is_none():
    assert _pct([], 0.5) is None


@pytest.mark.parametrize(
    ("q", "expected"),
    [(0.0, 1.0), (0.5, 3.0), (0.95, 5.0), (1.0, 5.0)],
)
def test_pct_indexes_the_sorted_values_and_clamps_at_the_top(q, expected):
    assert _pct([5.0, 1.0, 4.0, 2.0, 3.0], q) == expected


# ─── _summarize ──────────────────────────────────────────────────────────────


def test_summarize_groups_by_label():
    summary = _summarize([record(label="a"), record(label="b"), record(label="b", clip="c2")])
    assert sorted(summary) == [("unknown", "a"), ("unknown", "b")]
    assert summary[("unknown", "b")]["clips"] == 2


def test_wer_is_micro_averaged_over_edits_and_reference_words():
    # 1/2 and 3/10 edits: micro-average is 4/12, not the mean of the two rates.
    summary = _summarize(
        [
            record(clip="c1", wer_edits=1, ref_words=2),
            record(clip="c2", wer_edits=3, ref_words=10),
        ]
    )
    assert summary[UNKNOWN]["wer"] == pytest.approx(4 / 12)


def test_cer_is_micro_averaged_over_edits_and_reference_chars():
    summary = _summarize(
        [
            record(clip="c1", cer_edits=2, ref_chars=8),
            record(clip="c2", cer_edits=1, ref_chars=12),
        ]
    )
    assert summary[UNKNOWN]["cer"] == pytest.approx(3 / 20)


def test_nothing_to_score_reads_as_unscored_not_as_a_perfect_run():
    """No divide-by-zero, and no 0.00% either: a label with no reference words
    would otherwise print as flawless and rank first."""
    summary = _summarize([record(ref_words=0, ref_chars=0)])
    stats = summary[UNKNOWN]
    assert (stats["wer"], stats["cer"]) == (None, None)


def test_an_unscored_label_ranks_below_a_scored_one():
    summary = _summarize(
        [
            record(label="cold-only", clip="c1", ref_words=0, ref_chars=0),
            record(label="measured", clip="c2", wer_edits=3, ref_words=4),
        ]
    )
    assert [k[1] for k in ranked_labels(summary, "wer", {})] == ["measured", "cold-only"]


def test_an_unscored_label_renders_as_a_dash(capsys):
    print_overall(_summarize([record(ref_words=0, ref_chars=0)]))
    row = next(ln for ln in capsys.readouterr().out.splitlines() if record()["label"] in ln)
    assert "0.00" not in row
    assert "--" in row


def test_cold_runs_are_excluded_from_clip_and_accuracy_totals():
    summary = _summarize([record(cold=True, wer_edits=9, ref_words=9), record(cold=False)])
    stats = summary[UNKNOWN]
    assert stats["clips"] == 1
    assert stats["wer"] == 0.0


def test_cold_ready_is_the_worst_cold_load_and_warm_ready_the_median():
    summary = _summarize(
        [
            record(clip="c1", cold=True, time_to_ready=4.0),
            record(clip="c2", cold=True, time_to_ready=9.0),
            record(clip="c3", time_to_ready=0.1),
            record(clip="c4", time_to_ready=0.3),
        ]
    )
    stats = summary[UNKNOWN]
    assert stats["cold_ready"] == 9.0
    assert stats["warm_ready"] == 0.3


def test_missing_latencies_are_dropped_not_counted_as_zero():
    summary = _summarize(
        [
            record(clip="c1", finalize_latency=None, rtf=None),
            record(clip="c2", finalize_latency=0.5, rtf=0.5),
        ]
    )
    stats = summary[UNKNOWN]
    assert stats["median_final"] == 0.5
    assert stats["rtf"] == 0.5


def test_no_latencies_at_all_leaves_the_cells_empty():
    summary = _summarize([record(finalize_latency=None, rtf=None, time_to_ready=None)])
    stats = summary[UNKNOWN]
    assert (stats["median_final"], stats["p95_final"], stats["rtf"]) == (None, None, None)
    assert (stats["cold_ready"], stats["warm_ready"]) == (None, None)


def test_each_record_is_attributed_to_the_machine_that_produced_it():
    """Not "any record that carries provenance speaks for the label": on a
    leaderboard that would file one host's clips under another's row."""
    summary = _summarize(
        [
            record(clip="c1", provenance={"machine": "framework"}),
            record(clip="c2", provenance={"machine": "thinkpad"}),
        ]
    )
    assert sorted(summary) == [
        ("framework", UNKNOWN[1]),
        ("thinkpad", UNKNOWN[1]),
    ]


def test_a_row_with_no_provenance_groups_under_unknown():
    summary = _summarize([record(clip="c1"), record(clip="c2", provenance="not-a-dict")])
    assert summary[UNKNOWN]["machine"] == "unknown"


def test_audio_seconds_are_summed_over_warm_clips():
    summary = _summarize(
        [record(clip="c1", audio_seconds=1.5), record(clip="c2", audio_seconds=2.5)]
    )
    assert summary[UNKNOWN]["audio"] == pytest.approx(4.0)


# ─── _load_resources ─────────────────────────────────────────────────────────


def test_load_resources_is_empty_when_the_sidecar_is_absent(tmp_path):
    assert _load_resources(tmp_path / "absent.jsonl") == {}


def test_load_resources_indexes_peaks_by_label(tmp_path):
    path = tmp_path / "results-resources.jsonl"
    write_jsonl(path, [{"label": "a", "peak_rss_mb": 512.0, "peak_vram_mb": None}])
    assert _load_resources(path)[("unknown", "a")]["peak_rss_mb"] == 512.0


# ─── formatting helpers ──────────────────────────────────────────────────────


@pytest.mark.parametrize("value", [None, "n/a", float("nan")])
def test_f_renders_non_numbers_as_a_dash_placeholder(value):
    if isinstance(value, float):  # nan is a number: it formats normally
        assert _f(value).strip() == "nan"
    else:
        assert _f(value).strip() == "--"


def test_f_honours_the_format_spec():
    assert _f(1.239, "6.2f") == "  1.24"


@pytest.mark.parametrize(
    ("rtf", "expected"),
    [
        (0.01, "100x"),  # >= 10x renders without a decimal
        (0.5, "2.0x"),  # < 10x keeps one decimal
        (0.0, "--"),  # a zero rtf would divide by zero
        (-1.0, "--"),
        (None, "--"),
    ],
)
def test_speed_inverts_rtf_and_refuses_nonsense(rtf, expected):
    assert _speed(rtf).strip() == expected


# ─── table rendering ─────────────────────────────────────────────────────────


def test_overall_table_lists_every_label_sorted(capsys):
    print_overall(_summarize([record(label="zebra"), record(label="alpha")]))
    body = capsys.readouterr().out
    assert body.index("alpha") < body.index("zebra")


def test_overall_table_omits_the_machine_and_memory_columns_when_unknown(capsys):
    print_overall(_summarize([record()]))
    out = capsys.readouterr().out
    assert "machine" not in out
    assert "RSS MB" not in out


def test_overall_table_shows_memory_columns_once_peaks_are_attached(capsys):
    summary = _summarize([record(provenance={"machine": "thinkpad"})])
    summary[("thinkpad", UNKNOWN[1])]["peak_rss_mb"] = 800.0
    summary[("thinkpad", UNKNOWN[1])]["peak_vram_mb"] = 1200.0
    print_overall_of(summary)
    out = capsys.readouterr().out
    assert "RSS MB" in out and "VRAM MB" in out and "800.0" in out
    assert "machine" in out and "thinkpad" in out


def test_overall_table_of_an_empty_summary_still_prints_a_header(capsys):
    print_overall_of({})
    assert "label" in capsys.readouterr().out


def test_by_category_table_micro_averages_within_each_cell(capsys):
    print_by_category(
        [
            record(clip="c1", category="quiet", wer_edits=1, ref_words=4),
            record(clip="c2", category="quiet", wer_edits=1, ref_words=4),
            record(clip="c3", category="noise", wer_edits=0, ref_words=4),
        ]
    )
    out = capsys.readouterr().out
    assert "quiet" in out and "noise" in out
    assert "25.0" in out  # 2 edits / 8 words
    assert "0.0" in out


def test_by_category_ignores_cold_records(capsys):
    print_by_category([record(cold=True, category="quiet", wer_edits=4, ref_words=4)])
    assert "100.0" not in capsys.readouterr().out


def test_by_category_of_nothing_does_not_crash(capsys):
    print_by_category([])
    assert "WER% by category" in capsys.readouterr().out


def test_a_label_missing_a_category_renders_as_a_hole_not_a_zero(capsys):
    """A cell with no clips scored in it is unmeasured, and 0.0 would read as a
    perfect score for a category the label never attempted."""
    print_by_category(
        [
            record(label="a", clip="c1", category="quiet", wer_edits=1, ref_words=4),
            record(label="b", clip="c2", category="noise", wer_edits=1, ref_words=4),
        ]
    )
    out = capsys.readouterr().out
    lines = [ln for ln in out.splitlines() if ln.startswith(("a ", "b "))]
    assert len(lines) == 2
    assert all(len(ln.split()) == 3 for ln in lines)  # label + both category cells
    assert all("--" in ln for ln in lines)  # each label misses the other's category


# ─── cmd_summarize ───────────────────────────────────────────────────────────


class Args:
    def __init__(self, infile, by_category=False, sort="wer", corpus=None):
        self.infile = str(infile)
        self.by_category = by_category
        self.sort = sort
        self.corpus = corpus


def test_cmd_summarize_prints_the_record_count_and_the_table(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [{"type": "machine"}, record(clip="c1"), record(clip="c2")])
    cmd_summarize(Args(path))
    out = capsys.readouterr().out
    assert "2 records across 1 row(s) on 1 machine(s)" in out
    assert record()["label"] in out


def test_cmd_summarize_merges_the_resources_sidecar_next_to_the_results(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    write_jsonl(
        tmp_path / "results-resources.jsonl",
        [{"label": record()["label"], "peak_rss_mb": 640.5, "peak_vram_mb": 2048.0}],
    )
    cmd_summarize(Args(path))
    out = capsys.readouterr().out
    assert "640.5" in out and "2048.0" in out


def test_cmd_summarize_ignores_peaks_for_labels_not_in_the_results(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    write_jsonl(
        tmp_path / "results-resources.jsonl",
        [{"label": "some/other/target", "peak_rss_mb": 1.0, "peak_vram_mb": None}],
    )
    cmd_summarize(Args(path))
    assert "RSS MB" not in capsys.readouterr().out


def test_cmd_summarize_adds_the_category_breakdown_on_request(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    cmd_summarize(Args(path, by_category=True))
    assert "WER% by category" in capsys.readouterr().out


# ─── one_corpus ──────────────────────────────────────────────────────────────


def test_one_corpus_passes_a_single_corpus_through():
    records = [record(clip="c1"), record(clip="c2")]
    kept, corpus = one_corpus(records, None)
    assert corpus == "v1:testcorpus"
    assert len(kept) == 2


def test_two_corpora_in_one_file_is_refused_rather_than_micro_averaged():
    """A WER averaged across two corpora compares nothing: different audio,
    different reference text."""
    records = [record(clip="c1"), record(clip="c2", corpus_id="v1:other")]
    with pytest.raises(SystemExit, match="compares nothing"):
        one_corpus(records, None)


def test_naming_a_corpus_narrows_a_mixed_file():
    records = [record(clip="c1"), record(clip="c2", corpus_id="v1:other")]
    kept, corpus = one_corpus(records, "v1:other")
    assert corpus == "v1:other"
    assert [r["clip"] for r in kept] == ["c2"]


def test_records_with_no_corpus_id_are_refused():
    stale = record()
    del stale["corpus_id"]
    with pytest.raises(SystemExit, match="no corpus_id"):
        one_corpus([stale], None)


# ─── ranking and status ──────────────────────────────────────────────────────


def test_ranking_puts_the_lowest_wer_first():
    summary = _summarize(
        [
            record(label="bad", clip="c1", wer_edits=2, ref_words=4),
            record(label="good", clip="c2", wer_edits=0, ref_words=4),
        ]
    )
    assert ranked_labels(summary, "wer", {})[0][1] == "good"


def test_a_failed_label_sinks_below_every_clean_one_whatever_its_score():
    """Its WER was measured on however many clips it got through before
    failing, so it is not a comparable data point and must never outrank a
    target that actually finished."""
    summary = _summarize(
        [
            record(label="cut-short", clip="c1", wer_edits=0, ref_words=4),
            record(label="finished", clip="c2", wer_edits=2, ref_words=4),
        ]
    )
    statuses = {("unknown", "cut-short"): ("usability_fail", "exceeded budget")}
    assert [k[1] for k in ranked_labels(summary, "wer", statuses)] == ["finished", "cut-short"]


def test_status_records_are_read_out_of_the_results_file(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [record(), {"machine": "box", "label": "x", "status": "broken", "reason": "exited 1"}],
    )
    records, statuses = _load_latest(path)
    assert len(records) == 1
    assert statuses[("box", "x")] == ("broken", "exited 1")


def test_a_later_clean_run_clears_an_earlier_failure(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            {"machine": "unknown", "label": "x", "status": "usability_fail", "reason": "slow"},
            {"machine": "unknown", "label": "x", "status": "ok", "reason": ""},
        ],
    )
    assert _load_latest(path)[1][("unknown", "x")] == ("ok", "")


def test_cmd_summarize_flags_a_failed_label_in_the_table(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(),
            {
                "machine": "unknown",
                "label": record()["label"],
                "status": "usability_fail",
                "reason": "slow",
            },
        ],
    )
    cmd_summarize(Args(path))
    assert "USABILITY_FAIL" in capsys.readouterr().out
