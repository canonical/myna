"""Aggregate table for `myna-bench summarize`.

The table is what testers actually submit conclusions from, so the arithmetic
gets pinned here: last-write-wins de-duplication, micro-averaged (not
per-clip-averaged) WER, warm/cold separation, and the None-tolerant formatting
that keeps a partially-failed sweep readable instead of crashing the report.
"""

from __future__ import annotations

import json
import sys

import pytest
from _records import record

from myna.benchmarker._bootstrap import Interval, cell_intervals
from myna.benchmarker._summarize import (
    _f,
    _interval,
    _load_latest,
    _load_resources,
    _print_by_category,
    _print_overall,
    _rtfx,
    _speed,
    _summarize,
    _throttle,
    clip_samples,
    cmd_compare,
    cmd_summarize,
    gate,
    one_corpus,
    one_normalizer_version,
    print_intervals,
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


def test_load_latest_keeps_one_record_per_repeat(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(repeat=0), record(repeat=1), record(repeat=1, wer_edits=4)])
    records, _ = _load_latest(path)
    assert sorted((r["repeat"], r["wer_edits"]) for r in records) == [(0, 0), (1, 4)]


def test_load_latest_leaves_warmup_rows_out_of_every_aggregate(tmp_path):
    """Warmup rows stay in the file, as a record of what ran, and never score."""
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(clip="clip-0", phase="warmup", wer_edits=9, time_to_ready=5.0),
            record(clip="clip-1", phase="measured"),
        ],
    )
    records, _ = _load_latest(path)
    assert [r["phase"] for r in records] == ["measured"]


def test_a_warmup_row_does_not_shadow_the_measured_row_of_the_same_clip(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(phase="measured"), record(phase="warmup", wer_edits=9)])
    records, _ = _load_latest(path)
    assert [r["wer_edits"] for r in records] == [0]


def test_a_file_written_before_repeats_still_summarises(tmp_path):
    path = tmp_path / "results.jsonl"
    old = [{k: v for k, v in record().items() if k not in ("repeat", "phase")}]
    write_jsonl(path, old + [{**old[0], "cold": True}])
    records, _ = _load_latest(path)
    summary = _summarize(records)
    assert summary[UNKNOWN]["clips"] == 1
    assert summary[UNKNOWN]["cold_ready"] == 0.2


def test_repeats_pool_into_one_micro_average():
    summary = _summarize(
        [
            record(repeat=0, wer_edits=1, ref_words=10),
            record(repeat=1, wer_edits=3, ref_words=10),
        ]
    )
    assert summary[UNKNOWN]["wer"] == pytest.approx(0.2)
    assert summary[UNKNOWN]["clips"] == 2


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


def test_the_whisper_normalised_scores_micro_average_beside_ours():
    summary = _summarize(
        [
            record(clip="c1", wer_whisper_norm_edits=1, ref_words_whisper_norm=2),
            record(clip="c2", cer_whisper_norm_edits=3, ref_chars_whisper_norm=9),
        ]
    )
    assert summary[UNKNOWN]["wer_whisper"] == pytest.approx(1 / 4)
    assert summary[UNKNOWN]["cer_whisper"] == pytest.approx(3 / 20)
    assert summary[UNKNOWN]["wer"] == 0.0


def test_cold_rows_stay_out_of_the_whisper_normalised_scores():
    summary = _summarize(
        [
            record(clip="c1"),
            record(clip="c1", cold=True, wer_whisper_norm_edits=2, cer_whisper_norm_edits=5),
        ]
    )
    assert (summary[UNKNOWN]["wer_whisper"], summary[UNKNOWN]["cer_whisper"]) == (0.0, 0.0)


def test_a_cell_with_an_unscored_row_has_no_whisper_score():
    """Rows written before the secondary score existed carry no counts; a
    micro-average over the rest would describe a different set of clips."""
    stale = record(clip="c2")
    for field in [f for f in stale if "whisper" in f]:
        del stale[field]
    summary = _summarize([record(clip="c1"), stale])
    assert (summary[UNKNOWN]["wer_whisper"], summary[UNKNOWN]["cer_whisper"]) == (None, None)
    assert summary[UNKNOWN]["wer"] == 0.0


def test_the_table_labels_both_normalisers(capsys):
    print_overall(_summarize([record(wer_whisper_norm_edits=1, ref_words_whisper_norm=4)]))
    out = capsys.readouterr().out
    assert "WERw%" in out and "CERw%" in out and "25.00" in out
    assert "Whisper" in out and "EnglishTextNormalizer" in out


def test_nothing_to_score_reads_as_unscored_not_as_a_perfect_run():
    """No divide-by-zero, and no 0.00% either: a label with no reference words
    would otherwise print as flawless and rank first."""
    summary = _summarize(
        [record(ref_words=0, ref_chars=0, ref_words_whisper_norm=0, ref_chars_whisper_norm=0)]
    )
    stats = summary[UNKNOWN]
    assert (stats["wer"], stats["cer"]) == (None, None)
    assert (stats["wer_whisper"], stats["cer_whisper"]) == (None, None)


def test_an_unscored_label_ranks_below_a_scored_one():
    summary = _summarize(
        [
            record(label="cold-only", clip="c1", ref_words=0, ref_chars=0),
            record(label="measured", clip="c2", wer_edits=3, ref_words=4),
        ]
    )
    assert [k[1] for k in ranked_labels(summary, "wer", {})] == ["measured", "cold-only"]


def test_an_unscored_label_renders_as_a_dash(capsys):
    unscored = record(ref_words=0, ref_chars=0, ref_words_whisper_norm=0, ref_chars_whisper_norm=0)
    print_overall(_summarize([unscored]))
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


def test_a_starved_realtime_rows_latency_is_kept_out_but_its_accuracy_counts():
    """A feed that fell behind real time delivered a burst no microphone would,
    so its finalize latency is not live latency; the transcript is still one."""
    summary = _summarize(
        [
            record(clip="c1", finalize_latency=0.4, pace="realtime", pace_starved=False),
            record(clip="c2", finalize_latency=9.0, pace="realtime", pace_starved=True),
        ]
    )
    stats = summary[UNKNOWN]
    assert stats["median_final"] == 0.4
    assert stats["timed_clips"] == 1
    assert stats["clips"] == 2
    assert stats["starved"] == 1


def test_a_realtime_rows_rtf_is_the_pace_not_a_speed():
    """Fed on the capture clock, (terminal - ready) / audio cannot drop much
    below 1, so it would print as ~1x and rank the cell slowest."""
    summary = _summarize(
        [
            record(clip="c1", rtf=1.02, pace="realtime"),
            record(clip="c2", rtf=1.05, pace="realtime"),
        ]
    )
    assert summary[UNKNOWN]["rtf"] is None
    assert _speed(summary[UNKNOWN]["rtf"]).strip() == "--"


def test_a_max_rows_rtf_still_counts():
    assert _summarize([record(rtf=0.07, pace="max")])[UNKNOWN]["rtf"] == 0.07


def test_rows_from_before_the_pace_axis_are_not_starved():
    assert _summarize([record()])[UNKNOWN]["starved"] == 0


def test_the_table_warns_when_starved_latencies_were_left_out(capsys):
    summary = _summarize([record(pace="realtime", pace_starved=True)])
    print_overall(summary)
    assert "1 starved realtime clip(s)" in capsys.readouterr().out


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


def test_load_resources_skips_the_telemetry_trace(tmp_path):
    path = tmp_path / "results-resources.jsonl"
    write_jsonl(
        path,
        [
            {"label": "a", "kind": "cell", "peak_rss_mb": 512.0},
            {"label": "a", "kind": "sample", "t": 3.0, "rss_mb": 1.0},
        ],
    )
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
    def __init__(self, infile, by_category=False, sort="wer", corpus=None, ci=False):
        self.infile = str(infile)
        self.by_category = by_category
        self.sort = sort
        self.corpus = corpus
        self.ci = ci


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


def test_cmd_summarize_shows_energy_and_throttling_from_the_sidecar(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    write_jsonl(
        tmp_path / "results-resources.jsonl",
        [
            {
                "label": record()["label"],
                "kind": "cell",
                "peak_rss_mb": 640.5,
                "peak_vram_mb": None,
                "j_per_audio_s": 3.25,
                "throttled": {"cpu": None, "gpu": True},
            }
        ],
    )
    cmd_summarize(Args(path))
    out = capsys.readouterr().out
    assert "J/aud s" in out and "3.250" in out
    assert "throttle" in out and " gpu" in out


@pytest.mark.parametrize(
    ("throttled", "shown"),
    [
        ({"cpu": True, "gpu": True}, "cpu+gpu"),
        ({"cpu": False, "gpu": False}, "no"),
        ({"cpu": None, "gpu": None}, "--"),
        (None, "--"),
    ],
)
def test_throttle_cell_names_what_throttled(throttled, shown):
    assert _throttle(throttled) == shown


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


# ─── one_normalizer_version ─────────────────────────────────────────────────


def test_one_normalizer_version_passes_a_single_version_through():
    records = [record(clip="c1"), record(clip="c2")]
    one_normalizer_version(records)  # no raise


def test_mixed_normalizer_versions_are_refused():
    """A WER averaged across normalizer versions blends two scoring rules."""
    records = [record(clip="c1"), record(clip="c2", normalizer_version=2)]
    with pytest.raises(SystemExit, match="normalizer versions"):
        one_normalizer_version(records)


def test_missing_and_stamped_normalizer_versions_are_refused():
    stale = record(clip="c1")
    del stale["normalizer_version"]
    records = [stale, record(clip="c2")]
    with pytest.raises(SystemExit, match="normalizer versions"):
        one_normalizer_version(records)


def test_mixed_secondary_normalizer_versions_are_refused():
    records = [record(clip="c1"), record(clip="c2", secondary_normalizer_version="whisper-v1")]
    with pytest.raises(SystemExit, match="secondary normalizer versions"):
        one_normalizer_version(records)


def test_rows_without_a_secondary_score_do_not_block_the_primary_one():
    stale = record(clip="c2")
    del stale["secondary_normalizer_version"]
    one_normalizer_version([record(clip="c1"), stale])  # no raise


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


# ─── RTFx, sample floors, repeat noise ──────────────────────────────────────


def test_rtfx_is_total_audio_over_total_processing():
    summary = _summarize(
        [
            record(clip="c1", audio_seconds=10.0, rtf=0.1),  # 1 s of processing
            record(clip="c2", audio_seconds=2.0, rtf=0.5),  # 1 s of processing
        ]
    )
    assert summary[UNKNOWN]["rtfx"] == pytest.approx(6.0)


def test_a_realtime_rows_processing_is_left_out_of_rtfx():
    summary = _summarize(
        [
            record(clip="c1", audio_seconds=4.0, rtf=0.25, pace="max"),
            record(clip="c2", audio_seconds=4.0, rtf=1.02, pace="realtime"),
        ]
    )
    assert summary[UNKNOWN]["rtfx"] == pytest.approx(4.0)


def overall_row(rows: list, capsys) -> str:
    print_overall(_summarize(rows))
    return next(ln for ln in capsys.readouterr().out.splitlines() if UNKNOWN[1] in ln)


def latencies(n: int) -> list:
    """n clips with latencies 0.00, 0.01, ... so each percentile is distinct."""
    return [record(clip=f"c{i}", finalize_latency=i / 100) for i in range(n)]


def test_a_p95_from_fewer_than_60_latencies_is_refused(capsys):
    assert _summarize(latencies(59))[UNKNOWN]["p95_final"] is None
    assert overall_row(latencies(59), capsys).count("n too small") == 2  # p95 and p99


def test_a_p95_from_60_latencies_is_reported(capsys):
    summary = _summarize(latencies(60))
    assert summary[UNKNOWN]["p95_final"] == 0.57
    assert summary[UNKNOWN]["p99_final"] is None
    row = overall_row(latencies(60), capsys)
    assert "0.570" in row
    assert row.count("n too small") == 1


def test_a_single_latency_is_too_few_for_a_tail(capsys):
    assert overall_row(latencies(1), capsys).count("n too small") == 2


def test_no_latencies_is_a_dash_not_too_few(capsys):
    row = overall_row([record(finalize_latency=None)], capsys)
    assert "n too small" not in row


def test_an_unmeasured_rtfx_is_a_dash():
    assert _rtfx(None) == "--"


def test_the_table_prints_rtfx(capsys):
    assert " 6.0 " in overall_row([record(audio_seconds=3.0, rtf=1 / 6)], capsys)


def test_a_p99_needs_300_latencies():
    summary = _summarize([record(clip=f"c{i}", finalize_latency=0.3) for i in range(300)])
    assert summary[UNKNOWN]["p99_final"] == 0.3


def test_repeats_do_not_count_toward_the_latency_floor():
    """60 latencies from 20 clips still put the p95 on the slowest clip or two."""
    rows = [
        record(clip=f"c{i}", repeat=r, finalize_latency=0.3) for i in range(20) for r in range(3)
    ]
    assert _summarize(rows)[UNKNOWN]["p95_final"] is None


def test_repeats_of_enough_timed_clips_pool_into_the_tail():
    rows = [
        record(clip=f"c{i}", repeat=r, finalize_latency=0.01 * (3 * i + r))
        for i in range(60)
        for r in range(3)
    ]
    stats = _summarize(rows)[UNKNOWN]
    assert stats["timed_clips"] == 60
    # Nearest rank 171 of the 180 pooled latencies.
    assert stats["p95_final"] == pytest.approx(1.71)


def test_repeat_cv_is_the_spread_of_a_clips_repeated_latencies():
    rows = [
        record(clip="c1", repeat=0, finalize_latency=0.9),
        record(clip="c1", repeat=1, finalize_latency=1.1),
        record(clip="c1", repeat=2, finalize_latency=1.0),
    ]
    assert _summarize(rows)[UNKNOWN]["repeat_cv"] == pytest.approx(0.1)


def test_a_single_pass_has_no_repeat_cv():
    assert _summarize([record()])[UNKNOWN]["repeat_cv"] is None


def test_the_table_states_rtfx_is_at_batch_size_one(capsys):
    print_overall(_summarize([record()]))
    out = capsys.readouterr().out
    assert "RTFx" in out and "batch size 1" in out


# ─── clip samples ───────────────────────────────────────────────────────────


def test_clip_samples_fold_every_repeat_into_its_clip():
    rows = [
        record(clip="c1", repeat=0, wer_edits=1, ref_words=5, finalize_latency=0.2),
        record(clip="c1", repeat=1, wer_edits=2, ref_words=5, finalize_latency=0.4),
        record(clip="c2", repeat=0, wer_edits=0, ref_words=3, finalize_latency=None),
    ]
    samples = clip_samples(rows)
    assert samples["c1"].wer_edits == 3 and samples["c1"].ref_words == 10
    assert samples["c1"].latencies == (0.2, 0.4)
    assert samples["c2"].latencies == ()


def test_clip_samples_leave_starved_latency_and_realtime_throughput_out():
    rows = [
        record(clip="c1", pace="realtime", pace_starved=True, finalize_latency=9.0, rtf=1.0),
    ]
    sample = clip_samples(rows)["c1"]
    assert sample.latencies == ()
    assert sample.processing_seconds == 0.0
    assert sample.ref_words == 2


def test_clip_samples_skip_cold_rows():
    assert list(clip_samples([record(clip="c0", cold=True), record(clip="c1")])) == ["c1"]


def test_clip_samples_sum_the_character_counts_too():
    rows = [record(repeat=r, cer_edits=2, ref_chars=11) for r in range(2)]
    sample = clip_samples(rows)[record()["clip"]]
    assert (sample.cer_edits, sample.ref_chars) == (4, 22)


def test_clip_samples_sum_the_whisper_normalised_counts():
    rows = [record(repeat=r, wer_whisper_norm_edits=1, cer_whisper_norm_edits=3) for r in range(2)]
    sample = clip_samples(rows)[record()["clip"]]
    assert (sample.wer_whisper_edits, sample.ref_words_whisper) == (2, 4)
    assert (sample.cer_whisper_edits, sample.ref_chars_whisper) == (6, 22)
    assert sample.whisper_scored


@pytest.mark.parametrize("stale_first", [True, False])
def test_a_repeat_without_the_whisper_score_marks_its_clip_unscored(stale_first):
    stale = record(repeat=1)
    for field in [f for f in stale if "whisper" in f]:
        del stale[field]
    rows = [stale, record(repeat=0)] if stale_first else [record(repeat=0), stale]
    assert not clip_samples(rows)[record()["clip"]].whisper_scored


def test_clip_samples_read_a_row_without_a_cold_flag_as_warm():
    row = record()
    del row["cold"]
    assert list(clip_samples([row])) == [row["clip"]]


# ─── --ci ───────────────────────────────────────────────────────────────────


def test_cmd_summarize_prints_intervals_when_asked(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(clip=f"c{i}", wer_edits=i % 2) for i in range(10)])
    cmd_summarize(Args(path, ci=True))
    out = capsys.readouterr().out
    assert "95% CI" in out
    assert "10000 resamples" in out


def test_cmd_summarize_without_ci_prints_no_intervals(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    cmd_summarize(Args(path, ci=False))
    assert "95% CI" not in capsys.readouterr().out


def test_the_interval_table_brackets_each_estimate(capsys):
    """Identical clips: every resample equals the estimate, so the table is exact."""
    rows = [
        record(
            clip=f"c{i}",
            wer_edits=1,
            ref_words=10,
            cer_edits=1,
            ref_chars=20,
            audio_seconds=2.0,
            rtf=0.25,
            finalize_latency=0.3,
            wer_whisper_norm_edits=1,
            ref_words_whisper_norm=8,
            cer_whisper_norm_edits=1,
            ref_chars_whisper_norm=25,
        )
        for i in range(60)
    ]
    print_intervals(_summarize(rows), rows, [UNKNOWN], resamples=200)
    assert capsys.readouterr().out.splitlines()[2:] == [
        "label                                   WER%               CER%"
        "                 WERw%              CERw%"
        "            RTFx             med final             p95 final",
        "-" * 164,
        "whisper/cpu/tiny/batch  10.00 [10.00, 10.00]  5.00 [5.00, 5.00]"
        "  12.50 [12.50, 12.50]  4.00 [4.00, 4.00]"
        "  4.0 [4.0, 4.0]  0.300 [0.300, 0.300]  0.300 [0.300, 0.300]",
    ]


def test_the_interval_table_resamples_as_often_as_it_says(capsys):
    rows = [record(clip=f"c{i}", wer_edits=i % 3, ref_words=10) for i in range(30)]
    print_intervals(_summarize(rows), rows, [UNKNOWN], resamples=50)
    wer = cell_intervals(list(clip_samples(rows).values()), resamples=50)["wer"]
    line = capsys.readouterr().out.splitlines()[-1]
    assert f"[{wer.low * 100:.2f}, {wer.high * 100:.2f}]" in line


def test_the_interval_table_names_the_machine_once_there_are_two(capsys):
    rows = [
        record(label="a", provenance={"machine": "m1"}, finalize_latency=None),
        record(label="a", provenance={"machine": "m2"}, finalize_latency=0.3),
    ]
    summary = _summarize(rows)
    print_intervals(summary, rows, sorted(summary), resamples=50)
    out = capsys.readouterr().out.splitlines()
    assert out[1].startswith("95% CI: percentile bootstrap over clips (50 resamples, seed 0)")
    assert [ln.split()[:3] for ln in out[4:]] == [["a", "@", "m1"], ["a", "@", "m2"]]
    assert out[4].split()[-2:] == ["--", "--"]  # no latency: neither a value nor too few
    assert out[5].endswith("n too small")


def test_without_numpy_the_intervals_say_how_to_get_them(tmp_path, monkeypatch, capsys):
    monkeypatch.setitem(sys.modules, "numpy", None)
    monkeypatch.delitem(sys.modules, "myna.benchmarker._bootstrap", raising=False)
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record()])
    with pytest.raises(SystemExit, match=r"python3-numpy\), or pass --no-ci$"):
        cmd_summarize(Args(path, ci=True))
    assert capsys.readouterr().out == ""  # refused before the table, not after it
    cmd_summarize(Args(path, ci=False))
    assert record()["label"] in capsys.readouterr().out


# ─── compare ────────────────────────────────────────────────────────────────


class CompareArgs:
    def __init__(self, infile, first, second, corpus=None):
        self.infile = str(infile)
        self.first = first
        self.second = second
        self.corpus = corpus


def _pair(path, *, extra_edits):
    rows = []
    for i in range(40):
        rows.append(record(label="a", clip=f"c{i}", wer_edits=1, ref_words=10))
        rows.append(
            record(label="b", clip=f"c{i}", wer_edits=1 + extra_edits * (i % 2), ref_words=10)
        )
    write_jsonl(path, rows)


def test_compare_reports_the_delta_its_interval_and_p(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    _pair(path, extra_edits=2)
    cmd_compare(CompareArgs(path, "b", "a"))
    out = capsys.readouterr().out
    assert "b - a over 40 paired clip(s)" in out
    wer = next(ln for ln in out.splitlines() if ln.startswith("WER%"))
    assert "+10.00" in wer and "p=" in wer


def test_compare_prints_every_delta_exactly(tmp_path, capsys):
    """B is worse by one word, one character and 0.25 s on every clip, so
    every resample sees the same difference."""
    rows = []
    for i in range(10):
        rows.append(record(label="a", clip=f"c{i}", finalize_latency=0.5))
        rows.append(
            record(
                label="b",
                clip=f"c{i}",
                wer_edits=1,
                cer_edits=1,
                ref_chars=10,
                finalize_latency=0.75,
                wer_whisper_norm_edits=1,
                ref_words_whisper_norm=4,
                cer_whisper_norm_edits=1,
                ref_chars_whisper_norm=20,
            )
        )
    path = tmp_path / "results.jsonl"
    write_jsonl(path, rows)
    cmd_compare(CompareArgs(path, "b", "a"))
    assert capsys.readouterr().out.splitlines() == [
        "b - a over 10 paired clip(s), corpus v1:testcorpus",
        "WER%          +50.00 [+50.00, +50.00]  p=0.0001",
        "CER%          +10.00 [+10.00, +10.00]  p=0.0001",
        "WERw%         +25.00 [+25.00, +25.00]  p=0.0001",
        "CERw%         +5.00 [+5.00, +5.00]  p=0.0001",
        "med final s   +0.250 [+0.250, +0.250]  p=0.0001",
        "",
        "95% CI and two-sided p: paired bootstrap over clips (10000 resamples, seed 0);"
        " negative means b is lower.",
    ]


def test_compare_reads_a_label_that_itself_holds_an_at(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(label="a@x", provenance={"machine": "m1"}),
            record(label="b", provenance={"machine": "m1"}),
        ],
    )
    cmd_compare(CompareArgs(path, "a@x@m1", "b"))
    assert "over 1 paired clip(s)" in capsys.readouterr().out


def test_compare_marks_a_delta_it_cannot_measure(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [record(label="a", finalize_latency=None), record(label="b", finalize_latency=0.3)],
    )
    cmd_compare(CompareArgs(path, "a", "b"))
    line = next(ln for ln in capsys.readouterr().out.splitlines() if ln.startswith("med final"))
    assert line == "med final s   --  (0 timed on both)"


def test_compare_says_how_many_clips_the_latency_delta_used(tmp_path, capsys):
    rows = [record(label="a", clip=f"c{i}", finalize_latency=0.5) for i in range(3)]
    rows += [record(label="b", clip=f"c{i}", finalize_latency=0.5) for i in range(2)]
    rows.append(record(label="b", clip="c2", finalize_latency=None))
    path = tmp_path / "results.jsonl"
    write_jsonl(path, rows)
    cmd_compare(CompareArgs(path, "a", "b"))
    out = capsys.readouterr().out
    assert "over 3 paired clip(s)" in out
    assert "(2 timed on both)" in out


def test_an_interval_without_bounds_prints_its_estimate_alone():
    assert _interval(Interval(0.25, None, None), 100) == "25.00"


def test_compare_refuses_a_label_not_in_the_file(tmp_path):
    path = tmp_path / "results.jsonl"
    _pair(path, extra_edits=0)
    with pytest.raises(SystemExit, match="no rows labelled 'c'"):
        cmd_compare(CompareArgs(path, "a", "c"))


def test_compare_needs_a_machine_when_a_label_ran_on_two(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(label="a", provenance={"machine": "m1"}),
            record(label="a", provenance={"machine": "m2"}),
        ],
    )
    with pytest.raises(SystemExit, match="a@m1, a@m2"):
        cmd_compare(CompareArgs(path, "a", "a@m1"))


def test_compare_takes_label_at_machine(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(label="a", provenance={"machine": "m1"}, wer_edits=1),
            record(label="a", provenance={"machine": "m2"}, wer_edits=0),
        ],
    )
    cmd_compare(CompareArgs(path, "a@m1", "a@m2"))
    out = capsys.readouterr().out
    assert "a@m1 - a@m2 over 1 paired clip(s)" in out
    assert "WER%          +50.00" in out  # m1's row minus m2's, not the other way round


def test_compare_refuses_labels_with_no_clip_in_common(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(label="a", clip="c1"), record(label="b", clip="c2")])
    with pytest.raises(SystemExit, match="no clip"):
        cmd_compare(CompareArgs(path, "a", "b"))


def test_compare_picks_one_corpus_out_of_two(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(
        path,
        [
            record(label="a", corpus_id="v1:x"),
            record(label="b", corpus_id="v1:x"),
            record(label="b", clip="c2", corpus_id="v1:y"),
        ],
    )
    cmd_compare(CompareArgs(path, "a", "b", corpus="v1:x"))
    assert "corpus v1:x" in capsys.readouterr().out


def test_compare_refuses_two_corpora(tmp_path):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(label="a", corpus_id="v1:x"), record(label="b", corpus_id="v1:y")])
    with pytest.raises(SystemExit, match="compares nothing"):
        cmd_compare(CompareArgs(path, "a", "b"))


# ─── gate ────────────────────────────────────────────────────────────────────

PARAKEET = "myna-parakeet/cpu/int8/batch"


def test_gate_passes_rows_under_their_ceilings():
    records = [record(label=PARAKEET, language="de", wer_edits=1, ref_words=10)]
    assert gate(records, {}, {"myna-parakeet": {"de": 25}}) == []


def test_gate_fails_a_language_over_its_word_ceiling():
    records = [record(label=PARAKEET, language="de", wer_edits=3, ref_words=10)]
    assert gate(records, {}, {"myna-parakeet": {"de": 25}}) == [f"{PARAKEET} de: WER 30.0% > 25%"]


def test_gate_scores_chinese_by_characters():
    """Chinese has no word boundaries: its WER counts whole sentences."""
    records = [
        record(
            label="myna-funasr/cpu/sensevoice/batch",
            language="zh",
            wer_edits=1,
            ref_words=1,
            cer_edits=1,
            ref_chars=20,
        )
    ]
    assert gate(records, {}, {"myna-funasr": {"zh": 20}}) == []


def test_gate_fails_a_declared_language_nothing_scored():
    """A backend erroring on every clip leaves no rows, not a 0% pass."""
    records = [record(label=PARAKEET, language="de")]
    assert gate(records, {}, {"myna-parakeet": {"de": 25, "ru": 25}}) == [
        f"{PARAKEET} ru: no scored clips"
    ]


def test_gate_fails_a_backend_with_no_rows_at_all():
    assert gate([], {}, {"myna-whisper": {"en": 25}}) == ["myna-whisper: no scored clips"]


def test_gate_fails_a_row_that_did_not_finish():
    records = [record(label=PARAKEET, language="de")]
    statuses = {("box", PARAKEET): ("broken", "daemon died")}
    assert gate(records, statuses, {"myna-parakeet": {"de": 25}}) == [
        f"{PARAKEET}: broken (daemon died)"
    ]


def test_gate_ignores_cold_samples():
    records = [
        record(label=PARAKEET, language="de", wer_edits=0, ref_words=10),
        record(label=PARAKEET, language="de", clip="c2", cold=True, wer_edits=10, ref_words=10),
    ]
    assert gate(records, {}, {"myna-parakeet": {"de": 25}}) == []


def test_cmd_summarize_exits_1_on_a_gate_breach(tmp_path, capsys):
    path = tmp_path / "results.jsonl"
    write_jsonl(path, [record(label=PARAKEET, language="de", wer_edits=3, ref_words=10)])
    ceilings = tmp_path / "gate.yaml"
    ceilings.write_text("myna-parakeet: {de: 25}\n")
    args = Args(path, ci=False)
    args.gate = str(ceilings)
    with pytest.raises(SystemExit) as exc:
        cmd_summarize(args)
    assert exc.value.code == 1
    assert "WER 30.0% > 25%" in capsys.readouterr().out
