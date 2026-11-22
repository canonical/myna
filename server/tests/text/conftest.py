"""Shared fixtures of the myna-text tests."""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

from myna.text.protocol import Model, Refused

HERE = Path(__file__).parent
FAKE_BERGAMOT = HERE / "fake_bergamot.py"


class EchoModel(Model):
    """A model that upper-cases, for the protocol and server tests."""

    def __init__(self, id: str = "echo", task: str = "text.cleanup") -> None:
        self.id, self.task = id, task
        self.covers = {"language": ["en"]}
        self.accepts = ["instruction"]
        self.latency_ms = 5
        self.quality = 0.5
        self.warmed = 0
        self.closed = 0
        self.deadlines: list[tuple[str, float]] = []

    def warm(self, deadline_s: float) -> None:
        self.warmed += 1
        self.deadlines.append(("warm", deadline_s))

    def run(self, text: str, deadline_s: float) -> str:
        self.deadlines.append(("run", deadline_s))
        if text == "refuse":
            raise Refused("nope")
        if text == "crash":
            raise RuntimeError("boom with the dictated text")
        return text.upper()

    def close(self) -> None:
        self.closed += 1


@pytest.fixture
def fake_argv() -> list[str]:
    return [sys.executable, str(FAKE_BERGAMOT)]


def write_component(root: Path, name: str, meta: dict[str, object], files: tuple[str, ...]) -> Path:
    directory = root / name
    directory.mkdir(parents=True)
    (directory / "myna-text-model.json").write_text(json.dumps(meta))
    for file in files:
        (directory / file).write_text("x")
    return directory


TAGGER_META = {
    "kind": "disfluency-tagger",
    "id": "myna-disfluency-en",
    "task": "text.cleanup",
    "language": "en",
    "latency_ms": 10,
    "quality": 0.95,
    "tau": 0.9,
}

TRANSLATE_META = {
    "kind": "translate",
    "id": "bergamot-base-memory-en-fr",
    "task": "text.translate",
    "language": "en",
    "to": "fr",
    "latency_ms": 25,
    "quality": 0.7,
    "config": "config.yml",
}
