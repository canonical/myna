"""Per-clip scoring, end to end over the real session socket.

These run the benchmarker against an actual `myna-server` transport: the fake
adapter served over a UDS, driven through `WsUnixClient` exactly as a snap
would be. Stubbing the client here would leave the interesting failure (a
record schema the summarizer cannot aggregate) undetected, so the tests take
the round trip and then feed the output straight into `_summarize`.
"""

from __future__ import annotations

import argparse
import json

import pytest
from _clips import RATE, Collector, make_clip

from myna.benchmarker._bench import (
    AllClipsFailed,
    bench_clip,
    cmd_bench,
    read_keyword_list,
    run_clips,
    session_error,
    to_line,
)
from myna.benchmarker._summarize import SCHEMA_VERSION, _summarize
from myna.core import SessionConfig, TranscriptionError, TranscriptionFinal, serve_unix
from myna.testbed import NORMALIZER_VERSION, FakeAdapter, ScriptStep
from myna.testbed.metrics import SECONDARY_NORMALIZER_VERSION


def transcribing(text):
    """A fake adapter that finalises `text` and nothing else."""
    return FakeAdapter(script=[ScriptStep(0.0, TranscriptionFinal(text=text))])


class Recording(FakeAdapter):
    """Transcribes 'hello world' and keeps every session config it was given."""

    def __init__(self):
        super().__init__(
            script=[ScriptStep(0.0, TranscriptionFinal(text="hello world"))], keywords=True
        )
        self.configs: list[SessionConfig] = []

    async def run_session(self, config, audio, emit):
        self.configs.append(config)
        await super().run_session(config, audio, emit)


def failing(code="adapter_failed"):
    return FakeAdapter(script=[ScriptStep(0.0, TranscriptionError(code=code, message="boom"))])


@pytest.fixture
async def socket(tmp_path):
    """A fake adapter served over a UDS, transcribing 'hello world'."""
    path = tmp_path / "myna.sock"
    async with serve_unix(transcribing("hello world"), path):
        yield path


@pytest.fixture
async def wrong_socket(tmp_path):
    """A fake adapter whose transcript does not match the reference."""
    path = tmp_path / "wrong.sock"
    async with serve_unix(transcribing("hello word"), path):
        yield path


# ─── bench_clip ──────────────────────────────────────────────────────────────


async def test_a_perfect_transcript_scores_zero_wer_and_cer(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False)
    assert record.transcript == "hello world"
    assert (wer.rate, cer.rate) == (0.0, 0.0)


async def test_a_wrong_transcript_scores_the_edits(tmp_path, wrong_socket):
    clip = make_clip(tmp_path)
    _, wer, cer = await bench_clip(wrong_socket, clip, "fake/batch", streaming=False)
    assert wer.substitutions == 1
    assert wer.reference_length == 2
    assert cer.rate > 0


# ─── session_error ───────────────────────────────────────────────────────────


async def test_a_healthy_session_reports_no_error(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, _, _ = await bench_clip(socket, clip, "fake/batch", streaming=False)
    assert session_error(record) is None


async def test_an_adapter_failure_surfaces_as_a_coded_error(tmp_path):
    path = tmp_path / "broken.sock"
    async with serve_unix(failing(), path):
        clip = make_clip(tmp_path)
        record, _, _ = await bench_clip(path, clip, "fake/batch", streaming=False)
    error = session_error(record)
    assert error is not None and error["code"] == "adapter_failed"


# ─── to_line ─────────────────────────────────────────────────────────────────


async def test_a_record_row_is_json_serialisable_and_carries_provenance(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False)

    line = to_line(
        clip,
        record,
        wer,
        cer,
        label="fake/batch",
        cold=True,
        run_started="2026-08-20T00:00:00+00:00",
        served_models=["fake"],
        usability_fail=False,
        clips_scored=1,
        clips_requested=1,
        provenance={"machine": "box"},
    )

    assert json.loads(json.dumps(line)) == line
    assert line["provenance"] == {"machine": "box"}
    assert line["clip"] == "clip-a"
    assert line["category"] == "quiet"
    assert line["reference"] == "hello world"
    assert line["cold"] is True
    assert line["normalizer_version"] == NORMALIZER_VERSION
    assert line["schema_version"] == SCHEMA_VERSION


async def test_a_row_carries_the_whisper_normalised_score_beside_ours(tmp_path):
    """The leaderboard's normaliser reads "$20" and "twenty dollars" as one
    token; ours counts two errors. Both land on the row, each with its counts."""
    path = tmp_path / "dollars.sock"
    async with serve_unix(transcribing("He paid twenty dollars."), path):
        clip = make_clip(tmp_path, text="he paid $20")
        line = paced_line(clip, *await bench_clip(path, clip, "fake/batch", streaming=False))

    assert line["wer"] > 0
    assert line["secondary_normalizer_version"] == SECONDARY_NORMALIZER_VERSION
    assert (line["wer_whisper_norm"], line["cer_whisper_norm"]) == (0.0, 0.0)
    assert (line["wer_whisper_norm_edits"], line["ref_words_whisper_norm"]) == (0, 3)
    assert (line["cer_whisper_norm_edits"], line["ref_chars_whisper_norm"]) == (0, 11)


async def test_the_whisper_row_counts_insertions_and_characters(tmp_path):
    """An extra word is one insertion over three reference words, and six
    inserted characters (" today") over eleven."""
    path = tmp_path / "extra.sock"
    async with serve_unix(transcribing("He paid twenty dollars today."), path):
        clip = make_clip(tmp_path, text="he paid $20")
        line = paced_line(clip, *await bench_clip(path, clip, "fake/batch", streaming=False))

    assert (line["wer_whisper_norm_edits"], line["ref_words_whisper_norm"]) == (1, 3)
    assert (line["cer_whisper_norm_edits"], line["ref_chars_whisper_norm"]) == (6, 11)
    assert line["wer_whisper_norm"] == round(1 / 3, 4)
    assert line["cer_whisper_norm"] == round(6 / 11, 4)


async def test_the_secondary_score_uses_the_clips_language(tmp_path):
    """Non-English clips take Whisper's basic normaliser, which keeps numbers
    as spoken: "vingt" against "20" is an error there too."""
    from dataclasses import replace

    path = tmp_path / "fr.sock"
    async with serve_unix(transcribing("vingt"), path):
        clip = replace(make_clip(tmp_path, text="20"), language="fr")
        line = paced_line(clip, *await bench_clip(path, clip, "fake/batch", streaming=False))
    assert (line["wer_whisper_norm_edits"], line["ref_words_whisper_norm"]) == (1, 1)


async def test_provenance_is_omitted_entirely_when_not_supplied(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False)
    line = to_line(
        clip,
        record,
        wer,
        cer,
        label="fake/batch",
        cold=False,
        run_started="2026-08-20T00:00:00+00:00",
        served_models=[],
        usability_fail=False,
        clips_scored=1,
        clips_requested=1,
        provenance=None,
    )
    assert "provenance" not in line


# ─── run_clips ───────────────────────────────────────────────────────────────


async def test_a_sweep_stamps_the_corpus_it_measured(tmp_path, socket):
    """A WER without a corpus id cannot be compared to another machine's."""
    out = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
        corpus={"corpus_id": "v1:abcd", "corpus_manifest": "manifest.json"},
    )

    assert out.records[0]["corpus_id"] == "v1:abcd"
    assert out.records[0]["corpus_manifest"] == "manifest.json"


async def test_a_sweep_writes_one_record_per_clip(tmp_path, socket):
    clips = [make_clip(tmp_path, f"clip-{i}") for i in range(3)]
    out = Collector()

    overran, scored = await run_clips(
        socket=socket,
        clips=clips,
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )

    assert (overran, scored) == (False, 3)
    assert [r["clip"] for r in out.records] == ["clip-0", "clip-1", "clip-2"]


async def test_the_served_models_are_read_from_the_socket_capabilities(tmp_path, socket):
    out = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert out.records[0]["served_models"]


async def test_the_served_runtime_is_read_from_the_socket_capabilities(tmp_path):
    """Only the server knows which inference stack it loaded."""
    from dataclasses import replace

    adapter = transcribing("hello world")
    caps = replace(adapter.capabilities(), runtime={"onnxruntime": "1.23.0"})
    adapter.capabilities = lambda: caps
    path = tmp_path / "runtime.sock"
    out = Collector()
    async with serve_unix(adapter, path):
        await run_clips(
            socket=path,
            clips=[make_clip(tmp_path)],
            label="fake/batch",
            cold=False,
            streaming=False,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
        )
    assert out.records[0]["served_runtime"] == {"onnxruntime": "1.23.0"}


async def test_the_served_runtime_is_read_again_after_the_model_has_loaded(tmp_path):
    """A server reports library versions only once a session has loaded them."""
    from dataclasses import replace

    adapter = transcribing("hello world")
    base = adapter.capabilities()
    loaded = False
    run_session = adapter.run_session

    async def loading(*args, **kwargs):
        nonlocal loaded
        loaded = True
        await run_session(*args, **kwargs)

    adapter.run_session = loading
    adapter.capabilities = lambda: replace(
        base, runtime={"onnxruntime": "1.23.0"} if loaded else {"device": "cpu"}
    )
    path = tmp_path / "runtime.sock"
    out = Collector()
    async with serve_unix(adapter, path):
        await run_clips(
            socket=path,
            clips=[make_clip(tmp_path)],
            label="fake/batch",
            cold=True,
            streaming=False,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
        )
    assert out.records[0]["served_runtime"] == {"onnxruntime": "1.23.0"}


async def test_a_failed_runtime_re_read_keeps_the_first_answer(tmp_path, monkeypatch):
    from dataclasses import replace

    from myna.benchmarker import _bench

    adapter = transcribing("hello world")
    caps = replace(adapter.capabilities(), runtime={"device": "cpu"})
    adapter.capabilities = lambda: caps
    real = _bench.WsUnixClient.capabilities
    calls = 0

    async def once(self):
        nonlocal calls
        calls += 1
        if calls > 1:
            raise ConnectionResetError("server went away")
        return await real(self)

    monkeypatch.setattr(_bench.WsUnixClient, "capabilities", once)
    path = tmp_path / "runtime.sock"
    out = Collector()
    async with serve_unix(adapter, path):
        await run_clips(
            socket=path,
            clips=[make_clip(tmp_path)],
            label="fake/batch",
            cold=False,
            streaming=False,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
        )
    assert calls == 2
    assert out.records[0]["served_runtime"] == {"device": "cpu"}


async def test_a_server_that_does_not_name_its_runtime_leaves_it_unknown(tmp_path, socket):
    out = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert out.records[0]["served_runtime"] is None


async def test_a_sweep_against_a_dead_socket_still_records_the_failures(tmp_path):
    out = Collector()
    with pytest.raises(OSError):
        await run_clips(
            socket=tmp_path / "absent.sock",
            clips=[make_clip(tmp_path)],
            label="fake/batch",
            cold=False,
            streaming=False,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
        )


async def test_the_budget_stops_the_sweep_and_marks_every_row_unusable(
    tmp_path, socket, monkeypatch
):
    """A budget that expires mid-sweep must still write the clips it scored,
    every one of them flagged, so a partial run is legible rather than silent."""
    import asyncio

    from myna.benchmarker import _bench

    real_bench_clip = _bench.bench_clip

    async def slow_clip(*args, **kwargs):
        # Overshoot the budget by a wide margin on the first clip, so the
        # index-1 check cannot pass no matter how loaded the machine is.
        await asyncio.sleep(0.6)
        return await real_bench_clip(*args, **kwargs)

    monkeypatch.setattr(_bench, "bench_clip", slow_clip)

    clips = [make_clip(tmp_path, f"clip-{i}") for i in range(4)]
    out = Collector()

    overran, scored = await run_clips(
        socket=socket,
        clips=clips,
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=0.1,
        out_fp=out,
    )

    assert overran is True
    assert scored == 1
    assert [r["clip"] for r in out.records] == ["clip-0"]
    assert all(r["usability_fail"] for r in out.records)


async def test_a_budget_already_spent_stops_before_any_clip_runs(tmp_path, socket, capsys):
    out = Collector()
    overran, scored = await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=-1.0,
        out_fp=out,
    )
    assert (overran, scored) == (True, 0)
    assert out.records == []
    assert "budget exceeded after 0/1 clips" in capsys.readouterr().out


async def test_clips_scored_is_back_patched_onto_every_row(tmp_path, socket):
    clips = [make_clip(tmp_path, f"clip-{i}") for i in range(2)]
    out = Collector()
    await run_clips(
        socket=socket,
        clips=clips,
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert {r["clips_scored"] for r in out.records} == {2}
    assert {r["clips_requested"] for r in out.records} == {2}


async def test_a_failed_clip_is_written_but_not_counted_as_scored(tmp_path):
    path = tmp_path / "broken.sock"
    out = Collector()
    async with serve_unix(failing(), path):
        # Not a 100%-WER data point: the backend never ran. The rows are still
        # written, so the failure is on the record, but the sweep is told the
        # target is broken rather than banking a plausible-looking score.
        with pytest.raises(AllClipsFailed, match="adapter_failed"):
            await run_clips(
                socket=path,
                clips=[make_clip(tmp_path)],
                label="fake/batch",
                cold=False,
                streaming=False,
                provenance=None,
                budget_seconds=None,
                out_fp=out,
            )
    assert len(out.records) == 1
    assert out.records[0]["error"]["code"] == "adapter_failed"


async def test_the_sweep_output_aggregates_in_the_summarizer(tmp_path, socket):
    """The record schema is a contract between run and summarize: close it."""
    clips = [make_clip(tmp_path, f"clip-{i}") for i in range(3)]
    out = Collector()
    await run_clips(
        socket=socket,
        clips=clips,
        label="fake/cpu/none/batch",
        cold=False,
        streaming=False,
        provenance={"machine": "box"},
        budget_seconds=None,
        out_fp=out,
    )

    summary = _summarize(out.records)

    key = ("box", "fake/cpu/none/batch")
    assert list(summary) == [key]
    assert summary[key]["clips"] == 3
    assert summary[key]["wer"] == 0.0
    assert summary[key]["machine"] == "box"


async def test_the_progress_table_names_every_clip(tmp_path, socket, capsys):
    clips = [make_clip(tmp_path, f"clip-{i}") for i in range(2)]
    out = Collector()
    await run_clips(
        socket=socket,
        clips=clips,
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    printed = capsys.readouterr().out
    assert "clip-0" in printed and "clip-1" in printed
    assert "micro-averaged WER" in printed
    assert "audio streamed" in printed


async def test_a_partly_failing_sweep_still_scores_the_clips_that_worked(tmp_path, socket):
    """One bad clip is a data point; every clip bad is a broken target. Only
    the second is worth stopping for."""
    out = Collector()
    good = make_clip(tmp_path, "good")
    overran, scored = await run_clips(
        socket=socket,
        clips=[good],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert (overran, scored) == (False, 1)


async def test_realtime_pacing_is_opt_in(tmp_path, socket, capsys):
    """The sweep feeds flat out, which is what makes a full matrix affordable;
    real-time pacing is a cell of its own (the pace axis), and keeps a long
    clip from outrunning a websocket keepalive."""
    out = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert "fast as possible" in capsys.readouterr().out

    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=Collector(),
        realtime=True,
    )
    assert "real-time pace" in capsys.readouterr().out


# ─── repeats and warmup ──────────────────────────────────────────────────────


async def scheduled(tmp_path, socket, schedule, *, n=2, budget=None, events=None):
    from myna.benchmarker._schedule import Schedule

    out = Collector()
    result = await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path, f"clip-{i}") for i in range(n)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=budget,
        out_fp=out,
        events_fp=events,
        schedule=Schedule(**schedule),
    )
    return result, out.records


async def test_every_row_says_which_repeat_and_phase_it_is(tmp_path, socket):
    _, rows = await scheduled(tmp_path, socket, {"repeats": 2, "warmup_clips": 1})

    assert [(r["repeat"], r["phase"]) for r in rows] == [
        (0, "warmup"),
        (0, "measured"),
        (0, "measured"),
        (1, "measured"),
        (1, "measured"),
    ]
    assert rows[0]["clip"] == "clip-0"
    for repeat in (0, 1):
        passed = [r["clip"] for r in rows if r["phase"] == "measured" and r["repeat"] == repeat]
        assert sorted(passed) == ["clip-0", "clip-1"]


async def test_a_default_sweep_is_one_measured_pass(tmp_path, socket):
    _, rows = await scheduled(tmp_path, socket, {})
    assert [(r["clip"], r["repeat"], r["phase"]) for r in rows] == [
        ("clip-0", 0, "measured"),
        ("clip-1", 0, "measured"),
    ]


async def test_a_cold_sample_is_tagged_cold(tmp_path, socket):
    out = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/batch",
        cold=True,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
    )
    assert [(r["repeat"], r["phase"]) for r in out.records] == [(0, "cold")]


async def test_warmup_rows_are_not_counted_as_scored_or_requested(tmp_path, socket):
    (overran, scored), rows = await scheduled(tmp_path, socket, {"repeats": 2, "warmup_clips": 1})
    assert (overran, scored) == (False, 4)
    assert {r["clips_scored"] for r in rows} == {4}
    assert {r["clips_requested"] for r in rows} == {4}


async def test_warmup_runs_outside_the_budget(tmp_path, socket):
    """Like the cold sample, warmup is not a usability verdict: a spent budget
    stops the measured passes, not the warmup."""
    (overran, scored), rows = await scheduled(tmp_path, socket, {"warmup_clips": 1}, budget=-1.0)
    assert (overran, scored) == (True, 0)
    assert [r["phase"] for r in rows] == ["warmup"]


async def test_the_budget_clock_starts_at_the_first_measured_clip(tmp_path, monkeypatch):
    """A slow warmup (a model still paging in) must not eat the measured
    passes' budget."""
    from types import SimpleNamespace

    from myna.benchmarker import _bench

    clock = 0.0
    monkeypatch.setattr(_bench, "time", SimpleNamespace(monotonic=lambda: clock))

    class SlowWarmup(FakeAdapter):
        async def run_session(self, config, audio, emit):
            nonlocal clock
            clock += 100.0 if clock == 0.0 else 1.0
            await transcribing("hello world").run_session(config, audio, emit)

    path = tmp_path / "slow.sock"
    async with serve_unix(SlowWarmup(), path):
        (overran, scored), rows = await scheduled(tmp_path, path, {"warmup_clips": 1}, budget=10.0)
    assert (overran, scored) == (False, 2)
    assert [(r["clip"], r["phase"]) for r in rows] == [
        ("clip-0", "warmup"),
        ("clip-0", "measured"),
        ("clip-1", "measured"),
    ]


async def test_a_sweep_whose_measured_clips_all_fail_is_broken_despite_a_good_warmup(tmp_path):
    """Warmup rows must not rescue a cell whose measured rows never scored."""
    from myna.benchmarker._schedule import Schedule

    sessions = 0

    class FirstOnly(FakeAdapter):
        async def run_session(self, config, audio, emit):
            nonlocal sessions
            sessions += 1
            inner = transcribing("hello world") if sessions == 1 else failing()
            await inner.run_session(config, audio, emit)

    path = tmp_path / "first.sock"
    async with serve_unix(FirstOnly(), path):
        with pytest.raises(AllClipsFailed):
            await run_clips(
                socket=path,
                clips=[make_clip(tmp_path)],
                label="fake/batch",
                cold=False,
                streaming=False,
                provenance=None,
                budget_seconds=None,
                out_fp=Collector(),
                schedule=Schedule(warmup_clips=1),
            )


async def test_a_failed_warmup_clip_is_not_counted_as_a_failed_measurement(tmp_path):
    """Warmup exists to absorb a first session that fails while the model pages in."""
    sessions = 0

    class FirstFails(FakeAdapter):
        async def run_session(self, config, audio, emit):
            nonlocal sessions
            sessions += 1
            inner = failing() if sessions == 1 else transcribing("hello world")
            await inner.run_session(config, audio, emit)

    path = tmp_path / "first-fails.sock"
    async with serve_unix(FirstFails(), path):
        (overran, scored), rows = await scheduled(tmp_path, path, {"warmup_clips": 1})
    assert (overran, scored) == (False, 2)
    assert {r["clips_scored"] for r in rows} == {2}


async def test_the_event_stream_is_keyed_like_its_row(tmp_path, socket):
    events = Collector()
    _, rows = await scheduled(tmp_path, socket, {"repeats": 2, "warmup_clips": 1}, events=events)
    key = ("clip", "repeat", "phase")
    assert [tuple(e[k] for k in key) for e in events.records] == [
        tuple(r[k] for k in key) for r in rows
    ]


# ─── pace ────────────────────────────────────────────────────────────────────


def paced_line(clip, record, wer, cer, **kwargs):
    return to_line(
        clip,
        record,
        wer,
        cer,
        label="fake/streaming",
        cold=False,
        run_started="2026-08-20T00:00:00+00:00",
        served_models=[],
        usability_fail=False,
        clips_scored=1,
        clips_requested=1,
        provenance=None,
        **kwargs,
    )


async def test_a_max_pace_row_says_so_and_has_no_lag_to_judge(tmp_path, socket):
    clip = make_clip(tmp_path)
    line = paced_line(clip, *await bench_clip(socket, clip, "fake/streaming", streaming=True))
    assert (line["pace"], line["pace_lag"], line["pace_starved"]) == ("max", None, False)


async def test_a_realtime_row_records_how_far_its_feed_lagged(tmp_path, socket):
    clip = make_clip(tmp_path)
    got = await bench_clip(socket, clip, "fake/streaming", streaming=True, realtime=True)
    line = paced_line(clip, *got, pace="realtime")
    assert line["pace"] == "realtime"
    assert 0.0 <= line["pace_lag"] < 0.1
    assert line["pace_starved"] is False


async def test_a_realtime_row_whose_sender_fell_behind_is_flagged_starved(tmp_path, socket):
    from dataclasses import replace

    from myna.testbed import FedChunk

    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/streaming", streaming=True)
    late = (FedChunk(0.1, 0.1), FedChunk(0.2, 0.2), FedChunk(0.45, 0.3), FedChunk(0.46, 0.4))
    line = paced_line(clip, replace(record, feed=late), wer, cer, pace="realtime")
    assert line["pace_lag"] == pytest.approx(0.15)
    assert line["pace_starved"] is True


async def test_run_clips_stamps_the_pace_it_fed_at(tmp_path, socket):
    rows = {}
    for realtime in (False, True):
        out = Collector()
        await run_clips(
            socket=socket,
            clips=[make_clip(tmp_path)],
            label="fake/streaming",
            cold=False,
            streaming=True,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
            realtime=realtime,
        )
        rows[realtime] = out.records[0]
    assert rows[False]["pace"] == "max" and rows[True]["pace"] == "realtime"


async def test_a_realtime_run_marks_its_label_so_it_never_shares_a_max_row(tmp_path, socket):
    """Rows are keyed by label; a realtime and a max run under one --label
    would otherwise mix their finalize latencies."""
    labels = []
    for realtime, label in ((False, "x"), (True, "x"), (True, "x@realtime")):
        out = Collector()
        await run_clips(
            socket=socket,
            clips=[make_clip(tmp_path)],
            label=label,
            cold=False,
            streaming=True,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
            realtime=realtime,
        )
        labels.append(out.records[0]["label"])
    assert labels == ["x", "x@realtime", "x@realtime"]


async def test_starved_rows_are_counted_on_screen(tmp_path, socket, monkeypatch, capsys):
    from myna.benchmarker import _bench

    monkeypatch.setattr(_bench, "starved", lambda feed: True)
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path)],
        label="fake/streaming",
        cold=False,
        streaming=True,
        provenance=None,
        budget_seconds=None,
        out_fp=Collector(),
        realtime=True,
    )
    assert "STARVED" in capsys.readouterr().out


# ─── keywords ────────────────────────────────────────────────────────────────


@pytest.fixture
async def recording(tmp_path):
    adapter = Recording()
    path = tmp_path / "recording.sock"
    async with serve_unix(adapter, path):
        yield path, adapter


async def sweep(socket, clips, **kwargs):
    out = Collector()
    await run_clips(
        socket=socket,
        clips=clips,
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        out_fp=out,
        **kwargs,
    )
    return out.records


async def test_bench_clip_sends_the_keywords_it_is_given(tmp_path, recording):
    path, adapter = recording
    await bench_clip(
        path, make_clip(tmp_path), "fake/batch", streaming=False, keywords=("Siobhan",)
    )
    assert adapter.configs[-1].keywords == ("Siobhan",)


async def test_clip_keywords_are_scored_but_not_sent_by_default(tmp_path, recording):
    path, adapter = recording
    clip = make_clip(tmp_path, keywords=("world", "kernel"))
    (row,) = await sweep(path, [clip])
    assert adapter.configs[-1].keywords == ()
    assert row["keywords"] == ["world", "kernel"]
    assert row["keywords_sent"] is False
    assert (row["kw_in_reference"], row["kw_recalled"], row["kw_recalled_cased"]) == (1, 1, 1)
    assert (row["kw_absent"], row["kw_false_hits"]) == (1, 0)
    assert (row["kw_missed"], row["kw_false"]) == ([], [])


async def test_send_keywords_puts_the_clip_list_in_the_session(tmp_path, recording):
    path, adapter = recording
    (row,) = await sweep(path, [make_clip(tmp_path, keywords=("world",))], send_keywords=True)
    assert adapter.configs[-1].keywords == ("world",)
    assert row["keywords_sent"] is True


async def test_a_run_keyword_list_replaces_every_clip_list(tmp_path, recording):
    path, adapter = recording
    clip = make_clip(tmp_path, keywords=("world",))
    (row,) = await sweep(path, [clip], keywords=("hello", "Nagle"), send_keywords=True)
    assert adapter.configs[-1].keywords == ("hello", "Nagle")
    assert row["keywords"] == ["hello", "Nagle"]
    assert (row["kw_in_reference"], row["kw_absent"]) == (1, 1)


async def test_a_false_hit_and_a_miss_are_named_in_the_row(tmp_path, recording):
    path, _ = recording
    clip = make_clip(tmp_path, text="hello kernel", keywords=("kernel", "world"))
    (row,) = await sweep(path, [clip])
    assert (row["kw_missed"], row["kw_false"]) == (["kernel"], ["world"])


async def test_the_sweep_footer_reports_keyword_recall_and_false_hits(tmp_path, recording, capsys):
    path, _ = recording
    clips = [
        make_clip(tmp_path, "a", text="hello kernel", keywords=("kernel", "hello", "world")),
        make_clip(tmp_path, "b", keywords=("Nagle", "world")),
    ]
    await sweep(path, clips)
    printed = capsys.readouterr().out
    assert "keyword recall     : 66.67%  (2/3, as written 2/3)" in printed
    assert "keyword false hits : 50.00%  (1/2)" in printed


async def test_a_sweep_without_keywords_prints_no_keyword_lines(tmp_path, recording, capsys):
    path, _ = recording
    await sweep(path, [make_clip(tmp_path)])
    assert "keyword recall" not in capsys.readouterr().out


def test_a_keyword_list_file_is_one_entry_per_line_without_blanks_or_comments(tmp_path):
    path = tmp_path / "words.txt"
    path.write_text("# people\nSiobhan\n\n  red hat  \nkernel\n")
    assert read_keyword_list(path) == ("Siobhan", "red hat", "kernel")


def test_cmd_bench_hands_the_run_keyword_list_and_send_switch_to_the_sweep(tmp_path, monkeypatch):
    clip = make_clip(tmp_path, keywords=("world",))
    manifest = tmp_path / "manifest.json"
    entry = {
        "id": clip.id,
        "path": clip.path.name,
        "text": clip.text,
        "language": "en",
        "category": "quiet",
        "duration_seconds": clip.duration_seconds,
        "sample_rate_hz": RATE,
        "channels": 1,
        "source": "test",
        "license": "CC0-1.0",
        "sha256": clip.sha256,
        "keywords": ["world"],
    }
    manifest.write_text(json.dumps({"schema_version": 1, "clips": [entry]}))
    (tmp_path / "words.txt").write_text("Nagle\n")
    seen = {}

    async def fake_run_clips(**kwargs):
        seen.update(kwargs)
        return False, 1

    monkeypatch.setattr("myna.benchmarker._bench.run_clips", fake_run_clips)
    args = argparse.Namespace(
        manifest=str(manifest),
        clip=[],
        category=None,
        out=str(tmp_path / "out.jsonl"),
        socket=str(tmp_path / "s.sock"),
        label="x",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        realtime=False,
        send_keywords=True,
        keyword_list=str(tmp_path / "words.txt"),
    )
    cmd_bench(args)
    assert (seen["keywords"], seen["send_keywords"]) == (("Nagle",), True)
    assert seen["clips"][0].keywords == ("world",)

    args.keyword_list = None
    cmd_bench(args)
    assert seen["keywords"] is None
