"""The per-clip event sidecar: written by the sweep, read back unchanged."""

from __future__ import annotations

import gzip
import json
from pathlib import Path

import pytest
from _clips import Collector, make_clip

from myna.benchmarker._events import EventsFile, event_line, events_path_for, load_events
from myna.benchmarker._summarize import SCHEMA_VERSION
from myna.core import LoopbackClient, TranscriptionError, TranscriptionFinal, serve_unix
from myna.testbed import FakeAdapter, Harness, ScriptStep, TimedEvent


async def fake_session(tmp_path, clip_id="clip-a"):
    clip = make_clip(tmp_path, clip_id, seconds=0.5)
    adapter = FakeAdapter()  # the default script exercises every event type
    return await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=clip.open_source(),
    )


def write(path, *lines):
    fp = EventsFile(path)
    for line in lines:
        fp.write(line)
    fp.close()


def test_the_sidecar_sits_beside_the_results_file():
    assert events_path_for(Path("out/r.jsonl")) == Path("out/r-events.jsonl.gz")


async def test_a_sessions_events_round_trip_through_the_sidecar(tmp_path):
    record = await fake_session(tmp_path)
    path = tmp_path / "r-events.jsonl.gz"

    write(
        path,
        event_line(record, label="fake/batch", clip="clip-a", repeat=0, cold=True, machine="box"),
    )
    [loaded] = load_events(path)

    start = record.feed[0].t
    assert loaded.events == tuple(TimedEvent(te.t - start, te.event) for te in record.events)
    assert {te.event.type for te in loaded.events} >= {
        "transcription.progress",
        "transcription.final",
        "transcription.done",
    }
    assert [c.audio_end for c in loaded.feed] == [c.audio_end for c in record.feed]
    assert loaded.feed[0].t == 0.0
    assert loaded.audio_start == start
    assert loaded.audio_end == record.audio_end_t - start
    assert (loaded.machine, loaded.label, loaded.clip, loaded.repeat, loaded.cold) == (
        "box",
        "fake/batch",
        "clip-a",
        0,
        True,
    )
    assert loaded.started_at == record.started_at


async def test_appending_to_an_existing_sidecar_keeps_every_line(tmp_path):
    """--keep-results appends, which adds a gzip member rather than rewriting."""
    record = await fake_session(tmp_path)
    path = tmp_path / "r-events.jsonl.gz"
    kw = {"label": "fake/batch", "repeat": 0, "cold": False, "machine": None}
    write(path, event_line(record, clip="clip-a", **kw))
    write(path, event_line(record, clip="clip-b", **kw))

    assert [c.clip for c in load_events(path)] == ["clip-a", "clip-b"]


async def test_the_sidecar_is_stamped_with_the_schema_version(tmp_path):
    record = await fake_session(tmp_path)
    path = tmp_path / "r-events.jsonl.gz"
    write(path, event_line(record, label="l", clip="c", repeat=0, cold=False, machine=None))

    with gzip.open(path, "rt", encoding="utf-8") as fp:
        assert json.loads(fp.readline())["schema_version"] == SCHEMA_VERSION


def test_an_event_type_this_reader_does_not_know_is_skipped(tmp_path):
    """A newer server's event must not make an old file unreadable."""
    path = tmp_path / "r-events.jsonl.gz"
    line = {
        "schema_version": SCHEMA_VERSION,
        "machine": None,
        "label": "l",
        "clip": "c",
        "repeat": 0,
        "cold": False,
        "started_at": "2026-09-29T00:00:00+00:00",
        "audio_start": 0.001,
        "audio_end": None,
        "feed": [],
        "events": [[0.5, {"event": "transcription.future", "data": {}}]],
    }
    write(path, line)

    assert load_events(path)[0].events == ()


def test_a_session_that_sent_no_audio_is_timed_from_session_open():
    """With no chunk there is no audio start, so session open is the origin."""
    from myna.core import SessionConfig
    from myna.testbed import Candidate, ResultRecord
    from myna.testbed.harness import compute_metrics

    events = (TimedEvent(0.2, TranscriptionError(code="x")),)
    record = ResultRecord(
        candidate=Candidate(model="m", engine="e", streaming_strategy="batch"),
        config=SessionConfig(),
        started_at="2026-09-29T00:00:00+00:00",
        audio_duration_seconds=0.0,
        events=events,
        audio_end_t=None,
        metrics=compute_metrics(events, None),
        transcript="",
    )
    line = event_line(record, label="l", clip="c", repeat=0, cold=False, machine=None)
    assert line["audio_start"] == 0.0
    assert line["audio_end"] is None
    assert line["events"][0][0] == 0.2


# ─── the sweep writes it ─────────────────────────────────────────────────────


@pytest.fixture
async def socket(tmp_path):
    path = tmp_path / "myna.sock"
    adapter = FakeAdapter(script=[ScriptStep(0.0, TranscriptionFinal(text="hello world"))])
    async with serve_unix(adapter, path):
        yield path


async def test_a_sweep_writes_one_stream_per_row_under_the_rows_key(tmp_path, socket):
    from myna.benchmarker._bench import run_clips

    rows = Collector()
    events = Collector()
    await run_clips(
        socket=socket,
        clips=[make_clip(tmp_path, f"clip-{i}") for i in range(2)],
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance={"machine": "box"},
        budget_seconds=None,
        out_fp=rows,
        events_fp=events,
    )

    assert [(e["label"], e["clip"], e["repeat"]) for e in events.records] == [
        (r["label"], r["clip"], r.get("repeat", 0)) for r in rows.records
    ]
    assert {e["machine"] for e in events.records} == {"box"}
    assert all(e["events"] and e["feed"] for e in events.records)


async def test_the_bench_command_writes_the_sidecar_beside_its_results(tmp_path, socket):
    import argparse

    from myna.benchmarker._bench import cmd_bench
    from myna.testbed.corpus import stamp_corpus

    clip = make_clip(tmp_path)
    manifest = tmp_path / "manifest.json"
    entry = {
        "id": clip.id,
        "path": clip.path.name,
        "text": clip.text,
        "language": clip.language,
        "category": clip.category,
        "duration_seconds": clip.duration_seconds,
        "sample_rate_hz": clip.sample_rate_hz,
        "channels": clip.channels,
        "source": clip.source,
        "license": clip.license,
        "sha256": clip.sha256,
    }
    manifest.write_text(json.dumps({"schema_version": 1, "clips": [entry]}))
    stamp_corpus(manifest)
    out = tmp_path / "results.jsonl"
    args = argparse.Namespace(
        manifest=str(manifest),
        clip=[],
        category=None,
        out=str(out),
        socket=str(socket),
        label="fake/batch",
        cold=False,
        streaming=False,
        provenance=None,
        budget_seconds=None,
        realtime=False,
        keyword_list=None,
        send_keywords=False,
    )

    # cmd_bench drives its own event loop, so it runs off this test's loop.
    import asyncio

    await asyncio.to_thread(cmd_bench, args)

    assert [c.clip for c in load_events(events_path_for(out))] == [clip.id]


async def test_the_phase_round_trips_through_the_sidecar(tmp_path):
    record = await fake_session(tmp_path)
    path = tmp_path / "r-events.jsonl.gz"
    kw = {"label": "l", "clip": "c", "machine": None}
    write(
        path,
        event_line(record, repeat=0, cold=False, phase="warmup", **kw),
        event_line(record, repeat=2, cold=False, phase="measured", **kw),
        event_line(record, repeat=0, cold=True, **kw),
    )
    assert [(c.repeat, c.phase) for c in load_events(path)] == [
        (0, "warmup"),
        (2, "measured"),
        (0, "cold"),
    ]


def test_a_sidecar_written_before_phases_reads_its_phase_from_cold(tmp_path):
    path = tmp_path / "r-events.jsonl.gz"
    base = {
        "schema_version": SCHEMA_VERSION,
        "machine": None,
        "label": "l",
        "clip": "c",
        "repeat": 0,
        "started_at": "2026-09-29T00:00:00+00:00",
        "audio_start": 0.0,
        "audio_end": None,
        "feed": [],
        "events": [],
    }
    write(path, {**base, "cold": True}, {**base, "cold": False})
    assert [c.phase for c in load_events(path)] == ["cold", "measured"]
