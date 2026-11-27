"""Translation through ``myna-bergamot``, kept running between calls.

The model (Firefox Translations' en->fr student, run by bergamot-translator)
is loaded once by a child process that answers one line of text with one line
of translation, so a dictation pays the load only when it is warmed.
"""

from __future__ import annotations

import contextlib
import os
import select
import subprocess
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

from myna.text.components import Builder, require
from myna.text.protocol import Model, Params, Refused, declared

Spawn = Callable[[list[str]], Any]

#: What a warm-up translates: the first sentence is the slow one.
WARM_TEXT = "Hello."


def _spawn(argv: list[str]) -> Any:
    return subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        bufsize=0,
    )


class Worker:
    """One ``myna-bergamot`` child, spoken to a line at a time."""

    def __init__(self, argv: list[str], spawn: Spawn = _spawn) -> None:
        self._argv = argv
        self._spawn = spawn
        self._proc: Any = None
        self._pending = b""

    @property
    def alive(self) -> bool:
        return self._proc is not None and self._proc.poll() is None

    def _readline(self, deadline: float) -> str:
        """The next line from the child, or raise ``TimeoutError`` at ``deadline``."""
        stdout = self._proc.stdout
        while b"\n" not in self._pending:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([stdout], [], [], left)[0]:
                raise TimeoutError
            chunk = os.read(stdout.fileno(), 65536)
            if not chunk:
                raise ConnectionError("the translator exited")
            self._pending += chunk
        line, _, self._pending = self._pending.partition(b"\n")
        return line.decode("utf-8", "replace")

    def start(self, deadline_s: float) -> None:
        """Spawn the child and wait until it has built the model."""
        self.close()
        self._proc = self._spawn(self._argv)
        try:
            if self._readline(time.monotonic() + deadline_s) != "ready":
                raise ConnectionError("the translator did not say ready")
        except BaseException:
            self.close()
            raise

    def translate(self, text: str, deadline_s: float) -> str:
        # One line in, one line out: a line break would be two requests.
        line = " ".join(text.split())
        try:
            self._proc.stdin.write(line.encode("utf-8") + b"\n")
            return self._readline(time.monotonic() + deadline_s)
        except BaseException:
            # Late or broken either way: the next line would meet this answer.
            self.close()
            raise

    def close(self) -> None:
        proc, self._proc, self._pending = self._proc, None, b""
        if proc is None:
            return
        with contextlib.suppress(OSError):
            proc.stdin.close()
        with contextlib.suppress(OSError):
            proc.kill()
        with contextlib.suppress(OSError, subprocess.SubprocessError):
            proc.wait(timeout=5)
        with contextlib.suppress(OSError):
            proc.stdout.close()


class TranslateModel(Model):
    """A Bergamot model as a ``text.translate`` model."""

    accepts: list[str] = []

    def __init__(self, directory: Path, meta: Params, binary: Path, spawn: Spawn = _spawn) -> None:
        self.id = str(meta["id"])
        self.task = str(meta["task"])
        self.covers = {"language": [str(meta["language"])], "to": [str(meta["to"])]}
        self.latency_ms = int(meta["latency_ms"])
        self.quality = float(meta["quality"])
        self.declared = declared(meta)
        config = directory / str(meta["config"])
        self._worker = Worker([os.fspath(binary), "--model-config-paths", os.fspath(config)], spawn)
        self._lock = threading.Lock()

    def _ready(self, deadline_s: float) -> None:
        if not self._worker.alive:
            try:
                self._worker.start(deadline_s)
            except (OSError, ConnectionError, TimeoutError) as e:
                raise Refused("the translator did not start") from e

    def _translate(self, text: str, deadline_s: float) -> str:
        try:
            return self._worker.translate(text, deadline_s)
        except TimeoutError as e:
            raise Refused("timed out") from e
        except (OSError, ConnectionError) as e:
            raise Refused("the translator failed") from e

    def warm(self, deadline_s: float) -> None:
        with self._lock:
            started = time.monotonic()
            self._ready(deadline_s)
            self._translate(WARM_TEXT, max(deadline_s - (time.monotonic() - started), 0.001))

    def run(self, text: str, deadline_s: float) -> str:
        if not text.strip():
            return text
        with self._lock:
            started = time.monotonic()
            self._ready(deadline_s)
            return self._translate(text, max(deadline_s - (time.monotonic() - started), 0.001))

    def close(self) -> None:
        with self._lock:
            self._worker.close()


def kinds(binary: Path) -> dict[str, Builder]:
    """The component kinds this runtime serves, translating through ``binary``."""

    def build(directory: Path, meta: dict[str, Any]) -> Model:
        require(directory, str(meta["config"]))
        return TranslateModel(directory, meta, binary)

    return {"translate": build}
