"""Clip and sink fakes shared by the benchmarker tests that drive a socket."""

from __future__ import annotations

import wave

from myna.testbed.corpus import Clip, sha256_file

RATE = 16_000


class Collector:
    """The `out_fp` protocol run_clips writes through."""

    def __init__(self):
        self.records: list[dict] = []

    def write(self, record: dict) -> None:
        self.records.append(record)


def make_clip(
    tmp_path, clip_id="clip-a", text="hello world", seconds=0.4, category="quiet", keywords=()
):
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
        keywords=tuple(keywords),
    )
