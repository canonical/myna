"""Shared record factory for the benchmarker tests.

The benchmarker's JSONL schema is a contract shared with dev/matrix.py and
dev/aggregate.py, so the tests build records through one factory: a schema
change breaks here first, in one place, rather than in a dozen literals.
"""

from __future__ import annotations

BASE = {
    "error": None,
    "label": "whisper/cpu/tiny/batch",
    "cold": False,
    "clip": "clip-1",
    "category": "quiet",
    "language": "en",
    "reference": "hello world",
    "transcript": "hello world",
    "wer": 0.0,
    "cer": 0.0,
    "edits": {"sub": 0, "del": 0, "ins": 0},
    "wer_edits": 0,
    "ref_words": 2,
    "cer_edits": 0,
    "ref_chars": 11,
    "audio_seconds": 1.5,
    "time_to_first_event": 0.01,
    "time_to_ready": 0.2,
    "time_to_first_snippet": None,
    "time_to_first_final": 0.5,
    "time_to_first_committed": 0.5,
    "time_to_first_unstable": None,
    "time_to_terminal": 0.6,
    "finalize_latency": 0.3,
    "rtf": 0.25,
    "commit_stability": None,
    "committed_segments": 1,
    "streaming_strategy": "batch",
    "started_at": "2026-08-20T00:00:00+00:00",
    "run_started": "2026-08-20T00:00:00+00:00",
    "served_models": ["tiny"],
    "usability_fail": False,
    "clips_scored": 1,
    "clips_requested": 1,
}


def record(**overrides) -> dict:
    """A well-formed bench record with ``overrides`` applied."""
    return {**BASE, **overrides}
