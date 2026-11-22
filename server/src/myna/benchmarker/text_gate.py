"""Per-plugin gate configuration and the socket system for ``myna-bench text``.

A text plugin file (``<snap>-snap/plugins/<name>.toml``) may carry a ``[gate]``
table that tunes the translation gate of DECISIONS 7.4 for that language
pair; every key has a default, so a pair with no table gets the decided gate:

  vendor evidence   Mozilla's published FLORES COMET (``vendor_comet``) must
                    be >= ``comet_min`` (0.80); absent means "quality not
                    measured" and the pair needs the spot check all the same.
  chrF              >= ``chrf_ratio`` (0.85) x ``vendor_chrf`` where the vendor
                    published one, else >= ``chrf_floor`` (0.40);
                    ``chrf_min`` overrides both (en-fr keeps 0.58).
  degeneracy        empty 0, repeat loops <= 1%, median length ratio against
                    the reference within 0.5..2.0, copy-through <= 5%.
  latency           warm p95 < ``p95_max_ms`` (400).
"""

from __future__ import annotations

import json
import socket
import tomllib
from dataclasses import dataclass, fields
from pathlib import Path
from typing import Any

COMET_MIN = 0.80
CHRF_RATIO = 0.85
CHRF_FLOOR = 0.40


class GateError(Exception):
    """A plugin file or a provider that the evaluation cannot use."""


@dataclass(frozen=True)
class GateConfig:
    vendor_comet: float | None = None
    comet_min: float = COMET_MIN
    vendor_chrf: float | None = None
    chrf_ratio: float = CHRF_RATIO
    chrf_floor: float = CHRF_FLOOR
    chrf_min: float | None = None
    p95_max_ms: float = 400.0
    empty_max: float = 0.0
    repeat_max: float = 0.01
    ratio_min: float = 0.5
    ratio_max: float = 2.0
    copy_max: float = 0.05
    reverse: bool = False  # the plugin translates into English

    def chrf_limit(self) -> float:
        if self.chrf_min is not None:
            return self.chrf_min
        if self.vendor_chrf is not None:
            return self.chrf_ratio * self.vendor_chrf
        return self.chrf_floor

    @classmethod
    def from_plugin(cls, plugin: dict[str, Any]) -> GateConfig:
        table = dict(plugin.get("gate", {}))
        waiver = table.pop("spot_check_waiver", None)
        if waiver is not None and not (isinstance(waiver, str) and waiver.strip()):
            raise GateError("gate.spot_check_waiver must say why")
        known = {f.name for f in fields(cls)} - {"reverse"}
        unknown = sorted(set(table) - known)
        if unknown:
            raise GateError(f"unknown gate keys: {', '.join(unknown)}")
        for key, value in table.items():
            if isinstance(value, bool) or not isinstance(value, (int, float)):
                raise GateError(f"gate.{key} must be a number")
        reverse = plugin.get("to") == "en" and plugin.get("language") != "en"
        return cls(**{k: float(v) for k, v in table.items()}, reverse=reverse)


def load_plugin(path: Path) -> dict[str, Any]:
    plugin = tomllib.loads(path.read_text(encoding="utf-8"))
    plugin["name"] = path.stem
    plugin["path"] = str(path)
    return plugin


def find_plugin(repo: Path, ident: str) -> Path:
    """The plugin file whose ``id`` or file stem is ``ident`` (or the path itself)."""
    direct = Path(ident)
    if direct.suffix == ".toml" and direct.is_file():
        return direct
    for path in sorted(repo.glob("*-snap/plugins/*.toml")):
        if ident in (path.stem, tomllib.loads(path.read_text(encoding="utf-8")).get("id")):
            return path
    raise GateError(f"no plugin {ident!r} under {repo}/*-snap/plugins")


def pair_of(plugin: dict[str, Any]) -> str:
    """The non-English language of a translation plugin: its corpus slice suffix."""
    if plugin.get("task") != "text.translate":
        raise GateError(f"{plugin.get('id')}: not a translation plugin")
    src, dst = plugin["language"], plugin["to"]
    if "en" not in (src, dst) or src == dst:
        raise GateError(
            f"{plugin['id']}: {src}->{dst}: the corpus is English to X and X to English"
        )
    return str(dst if src == "en" else src)


def _call(path: str, request: dict[str, Any], timeout: float = 30.0) -> dict[str, Any]:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
        sock.settimeout(timeout)
        sock.connect(path)
        sock.sendall(json.dumps(request).encode() + b"\n")
        reply: dict[str, Any] = json.loads(sock.makefile("rb").readline())
    return reply


def socket_system(path: str, model: str, deadline_ms: int = 5000) -> Any:
    """A ``text_suite.System`` backed by one model of a running provider."""
    from myna.benchmarker.text_suite import System

    caps = _call(path, {"op": "capabilities"})
    entry = next((m for m in caps.get("models", []) if m["id"] == model), None)
    if entry is None:
        raise GateError(f"{path} does not offer model {model!r}")
    params = {k: v[0] for k, v in entry["covers"].items()}
    task = entry["task"]
    warm = _call(path, {"op": "warm", "model": model, "task": task, "params": params})
    if "error" in warm:
        raise GateError(f"{model}: warm refused: {warm['error']}")

    def run(text: str) -> str:
        reply = _call(
            path,
            {
                "op": "run",
                "model": model,
                "task": task,
                "params": params,
                "text": text,
                "deadline_ms": deadline_ms,
            },
        )
        if "text" not in reply:
            raise GateError(f"{model}: run refused: {reply.get('error', reply)}")
        return str(reply["text"])

    if task == "text.translate":
        return System(f"socket:{model}", translate=run)
    if task == "text.cleanup":
        return System(f"socket:{model}", tidy=run)
    raise GateError(f"{model}: task {task} has no text suite")
