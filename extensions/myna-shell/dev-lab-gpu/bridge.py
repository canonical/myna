"""bridge.py — talk to bridge.js.

The JS side owns the shader, the model and the uniform packing; this is just
the pipe. A long-lived `gjs --serve` subprocess is used rather than one
process per frame because the lab is interactive: startup is ~50ms, a
request/reply over a pipe is well under a millisecond, so a 60fps slider
drag stays responsive while every frame still comes from the real JS model.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

HERE = Path(__file__).resolve().parent
BRIDGE_JS = HERE / "bridge.js"


class BridgeError(RuntimeError):
    """The JS side failed, or stopped answering."""


def load_shader() -> dict:
    """Fetch the generated shader and uniform list (a one-shot call)."""
    result = subprocess.run(
        ["gjs", "-m", str(BRIDGE_JS), "--shader"],
        capture_output=True, text=True, cwd=HERE,
    )
    if result.returncode != 0:
        raise BridgeError(f"bridge.js --shader failed:\n{result.stderr}")
    return json.loads(result.stdout)


class RibbonModel:
    """A live `gjs --serve` process, one JSON line per frame."""

    def __init__(self) -> None:
        self._proc = subprocess.Popen(
            ["gjs", "-m", str(BRIDGE_JS), "--serve"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            text=True, bufsize=1, cwd=HERE,
            # stderr is left attached to our own terminal so a JS exception
            # is seen immediately rather than swallowed into a pipe nobody
            # drains.
            stderr=None,
            env={**os.environ, "G_MESSAGES_DEBUG": ""},
        )

    def frame(self, **request) -> dict:
        """Compute one frame's uniforms.

        Keyword arguments are passed straight through to `computeRibbonModel`
        (envelope, elapsedMs, phase, phaseElapsedMs, reducedMotion,
        severityTint) plus width/height/palette.
        """
        if self._proc.poll() is not None:
            raise BridgeError("bridge.js exited; see stderr above")
        self._proc.stdin.write(json.dumps(request) + "\n")
        self._proc.stdin.flush()
        line = self._proc.stdout.readline()
        if not line:
            raise BridgeError("bridge.js closed its output; see stderr above")
        response = json.loads(line)
        if "error" in response:
            raise BridgeError(response["error"])
        return response

    def close(self) -> None:
        if self._proc.poll() is None:
            self._proc.stdin.close()
            try:
                self._proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self._proc.kill()

    def __enter__(self) -> "RibbonModel":
        return self

    def __exit__(self, *exc) -> None:
        self.close()
