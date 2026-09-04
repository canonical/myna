"""Per-clip scoring, end to end over the real session socket.

These run the benchmarker against an actual `myna-server` transport: the fake
adapter served over a UDS, driven through `WsUnixClient` exactly as a snap
would be. Stubbing the client here would leave the interesting failure (a
record schema the summarizer cannot aggregate) undetected, so the tests take
the round trip and then feed the output straight into `_summarize`.
"""

from __future__ import annotations

import json
import wave

import pytest

from myna.benchmarker._bench import bench_clip, run_clips, session_error, to_line
from myna.benchmarker._summarize import _summarize
from myna.core import TranscriptionError, TranscriptionFinal, serve_unix
from myna.testbed import FakeAdapter, ScriptStep
from myna.testbed.corpus import Clip, sha256_file

RATE = 16_000


class Collector:
    """The `out_fp` protocol run_clips writes through."""

    def __init__(self):
        self.records: list[dict] = []

    def write(self, record: dict) -> None:
        self.records.append(record)


def make_clip(tmp_path, clip_id="clip-a", text="hello world", seconds=0.4, category="quiet"):
    path = tmp_path / f"{clip_id}.wav"
    with wave.open(str(path), "w") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(RATE)
        wf.writeframes(b"\x00\x00" * int(RATE * seconds))
    return Clip(
        id=clip_id,
        path=path,
        text=text,
        language="en",
        category=category,
        duration_seconds=seconds,
        sample_rate_hz=RATE,
        channels=1,
        source="test",
        license="CC0-1.0",
        sha256=sha256_file(path),
    )


def transcribing(text):
    """A fake adapter that finalises `text` and nothing else."""
    return FakeAdapter(script=[ScriptStep(0.0, TranscriptionFinal(text=text))])


def failing(code="adapter_failed"):
    return FakeAdapter(script=[ScriptStep(0.0, TranscriptionError(code=code, message="boom"))])


@pytest.fixture
async def socket(tmp_path):
    """A fake adapter served over a UDS, transcribing 'hello world'."""
    path = tmp_path / "ubustt.sock"
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
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False, batch=True)
    assert record.transcript == "hello world"
    assert (wer.rate, cer.rate) == (0.0, 0.0)


async def test_a_wrong_transcript_scores_the_edits(tmp_path, wrong_socket):
    clip = make_clip(tmp_path)
    _, wer, cer = await bench_clip(wrong_socket, clip, "fake/batch", streaming=False, batch=True)
    assert wer.substitutions == 1
    assert wer.reference_length == 2
    assert cer.rate > 0


# ─── session_error ───────────────────────────────────────────────────────────


async def test_a_healthy_session_reports_no_error(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, _, _ = await bench_clip(socket, clip, "fake/batch", streaming=False, batch=True)
    assert session_error(record) is None


async def test_an_adapter_failure_surfaces_as_a_coded_error(tmp_path):
    path = tmp_path / "broken.sock"
    async with serve_unix(failing(), path):
        clip = make_clip(tmp_path)
        record, _, _ = await bench_clip(path, clip, "fake/batch", streaming=False, batch=True)
    error = session_error(record)
    assert error is not None and error["code"] == "adapter_failed"


# ─── to_line ─────────────────────────────────────────────────────────────────


async def test_a_record_row_is_json_serialisable_and_carries_provenance(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False, batch=True)

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


async def test_provenance_is_omitted_entirely_when_not_supplied(tmp_path, socket):
    clip = make_clip(tmp_path)
    record, wer, cer = await bench_clip(socket, clip, "fake/batch", streaming=False, batch=True)
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
        overran, scored = await run_clips(
            socket=path,
            clips=[make_clip(tmp_path)],
            label="fake/batch",
            cold=False,
            streaming=False,
            provenance=None,
            budget_seconds=None,
            out_fp=out,
        )
    assert (overran, scored) == (False, 0)
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

    assert list(summary) == ["fake/cpu/none/batch"]
    assert summary["fake/cpu/none/batch"]["clips"] == 3
    assert summary["fake/cpu/none/batch"]["wer"] == 0.0
    assert summary["fake/cpu/none/batch"]["machine"] == "box"


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
