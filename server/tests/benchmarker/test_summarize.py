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
)


def write_jsonl(path, records) -> None:
    path.write_text("".join(json.dumps(r) + "\n" for r in records), encoding="utf-8")


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
    (loaded,) = _load_latest(path)
    assert loaded["transcript"] == "second"


def test_load_latest_separates_cold_from_warm_for_the_same_clip(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(cold=True), record(cold=False)])
    assert {r["cold"] for r in _load_latest(path)} == {True, False}


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
    assert [r["clip"] for r in _load_latest(path)] == ["good"]


def test_load_latest_tolerates_blank_lines(tmp_path):
    path = tmp_path / "results.jsonl"
    path.write_text(json.dumps(record()) + "\n\n   \n", encoding="utf-8")
    assert len(_load_latest(path)) == 1


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
    assert sorted(summary) == ["a", "b"]
    assert summary["b"]["clips"] == 2


def test_wer_is_micro_averaged_over_edits_and_reference_words():
    # 1/2 and 3/10 edits: micro-average is 4/12, not the mean of the two rates.
    summary = _summarize(
        [
            record(clip="c1", wer_edits=1, ref_words=2),
            record(clip="c2", wer_edits=3, ref_words=10),
        ]
    )
    assert summary[record()["label"]]["wer"] == pytest.approx(4 / 12)


def test_cer_is_micro_averaged_over_edits_and_reference_chars():
    summary = _summarize(
        [
            record(clip="c1", cer_edits=2, ref_chars=8),
            record(clip="c2", cer_edits=1, ref_chars=12),
        ]
    )
    assert summary[record()["label"]]["cer"] == pytest.approx(3 / 20)


def test_zero_reference_length_does_not_divide_by_zero():
    summary = _summarize([record(ref_words=0, ref_chars=0)])
    stats = summary[record()["label"]]
    assert (stats["wer"], stats["cer"]) == (0.0, 0.0)


def test_cold_runs_are_excluded_from_clip_and_accuracy_totals():
    summary = _summarize([record(cold=True, wer_edits=9, ref_words=9), record(cold=False)])
    stats = summary[record()["label"]]
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
    stats = summary[record()["label"]]
    assert stats["cold_ready"] == 9.0
    assert stats["warm_ready"] == 0.3


def test_missing_latencies_are_dropped_not_counted_as_zero():
    summary = _summarize(
        [
            record(clip="c1", finalize_latency=None, rtf=None),
            record(clip="c2", finalize_latency=0.5, rtf=0.5),
        ]
    )
    stats = summary[record()["label"]]
    assert stats["median_final"] == 0.5
    assert stats["rtf"] == 0.5


def test_no_latencies_at_all_leaves_the_cells_empty():
    summary = _summarize([record(finalize_latency=None, rtf=None, time_to_ready=None)])
    stats = summary[record()["label"]]
    assert (stats["median_final"], stats["p95_final"], stats["rtf"]) == (None, None, None)
    assert (stats["cold_ready"], stats["warm_ready"]) == (None, None)


def test_machine_is_taken_from_provenance_when_any_record_carries_it():
    summary = _summarize(
        [
            record(clip="c1"),
            record(clip="c2", provenance={"machine": "thinkpad"}),
        ]
    )
    assert summary[record()["label"]]["machine"] == "thinkpad"


def test_machine_is_none_when_provenance_is_absent_or_malformed():
    summary = _summarize([record(clip="c1"), record(clip="c2", provenance="not-a-dict")])
    assert summary[record()["label"]]["machine"] is None


def test_audio_seconds_are_summed_over_warm_clips():
    summary = _summarize(
        [record(clip="c1", audio_seconds=1.5), record(clip="c2", audio_seconds=2.5)]
    )
    assert summary[record()["label"]]["audio"] == pytest.approx(4.0)


# ─── _load_resources ─────────────────────────────────────────────────────────


def test_load_resources_is_empty_when_the_sidecar_is_absent(tmp_path):
    assert _load_resources(tmp_path / "absent.jsonl") == {}


def test_load_resources_indexes_peaks_by_label(tmp_path):
    path = tmp_path / "results-resources.jsonl"
    write_jsonl(path, [{"label": "a", "peak_rss_mb": 512.0, "peak_vram_mb": None}])
    assert _load_resources(path)["a"]["peak_rss_mb"] == 512.0


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
    _print_overall(_summarize([record(label="zebra"), record(label="alpha")]))
    body = capsys.readouterr().out
    assert body.index("alpha") < body.index("zebra")


def test_overall_table_omits_the_machine_and_memory_columns_when_unknown(capsys):
    _print_overall(_summarize([record()]))
    out = capsys.readouterr().out
    assert "machine" not in out
    assert "RSS MB" not in out


def test_overall_table_shows_memory_columns_once_peaks_are_attached(capsys):
    summary = _summarize([record(provenance={"machine": "thinkpad"})])
    summary[record()["label"]]["peak_rss_mb"] = 800.0
    summary[record()["label"]]["peak_vram_mb"] = 1200.0
    _print_overall(summary)
    out = capsys.readouterr().out
    assert "RSS MB" in out and "VRAM MB" in out and "800.0" in out
    assert "machine" in out and "thinkpad" in out


def test_overall_table_of_an_empty_summary_still_prints_a_header(capsys):
    _print_overall({})
    assert "label" in capsys.readouterr().out


def test_by_category_table_micro_averages_within_each_cell(capsys):
    _print_by_category(
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
    _print_by_category([record(cold=True, category="quiet", wer_edits=4, ref_words=4)])
    assert "100.0" not in capsys.readouterr().out


def test_by_category_of_nothing_does_not_crash(capsys):
    _print_by_category([])
    assert "WER% by category" in capsys.readouterr().out


def test_a_label_missing_a_category_renders_as_zero_not_a_hole(capsys):
    _print_by_category(
        [
            record(label="a", clip="c1", category="quiet", wer_edits=1, ref_words=4),
            record(label="b", clip="c2", category="noise", wer_edits=1, ref_words=4),
        ]
    )
    lines = [ln for ln in capsys.readouterr().out.splitlines() if ln.startswith(("a ", "b "))]
    assert len(lines) == 2
    assert all(len(ln.split()) == 3 for ln in lines)  # label + both category cells


# ─── cmd_summarize ───────────────────────────────────────────────────────────


class Args:
    def __init__(self, infile, by_category=False):
        self.infile = str(infile)
        self.by_category = by_category


def test_cmd_summarize_prints_the_record_count_and_the_table(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [{"type": "machine"}, record(clip="c1"), record(clip="c2")])
    cmd_summarize(Args(path))
    out = capsys.readouterr().out
    assert "2 records across 1 label(s)" in out
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
