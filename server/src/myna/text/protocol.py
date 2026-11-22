"""The ``myna-text-v1`` provider: three calls, one JSON object per line.

``capabilities`` lists the models; ``warm`` loads one so the first ``run`` does
not pay for it; ``run`` applies one to a piece of text. A reply with an
``error`` is a refusal and says nothing about the text, which may be private.
"""

from __future__ import annotations

import abc
import logging
import time
from collections.abc import Callable, Mapping
from typing import Any

PROTOCOL = "myna-text-v1"

#: The manifest and capabilities contract this provider speaks (see
#: ``.kb/text-plugin-contract.md``); a manifest naming a higher one is skipped.
CONTRACT = 2

#: The class of a model whose manifest names none. A class this client does
#: not know makes the model uncovered, so the table only fills the gap.
DEFAULT_CLASS = {
    "text.cleanup": "edit",
    "text.translate": "translate",
    "text.rewrite": "rewrite",
}

NEEDS = ("sentence", "dictation")
PARAM_KINDS = ("language", "text")
PARAM_ROLES = ("source", "target", "free")

log = logging.getLogger("myna.text")

Params = Mapping[str, Any]


class Refused(Exception):
    """A call the provider will not carry out; the message is the reply's error."""


def declared(meta: Params) -> dict[str, Any]:
    """The contract 2 fields of a manifest, checked; absent ones are left out.

    Raises ``ValueError`` for a manifest of a newer contract or a field of the
    wrong shape, so the component is skipped rather than offered wrongly.
    """
    contract = meta.get("contract", 1)
    if not isinstance(contract, int) or isinstance(contract, bool) or not 1 <= contract <= CONTRACT:
        raise ValueError("contract")
    out: dict[str, Any] = {}
    for key in ("class", "license", "attribution", "source"):
        if key in meta:
            if not isinstance(meta[key], str) or not meta[key]:
                raise ValueError(key)
            out[key] = meta[key]
    if "needs" in meta:
        if meta["needs"] not in NEEDS:
            raise ValueError("needs")
        out["needs"] = meta["needs"]
    if "download_bytes" in meta:
        size = meta["download_bytes"]
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            raise ValueError("download_bytes")
        out["download_bytes"] = size
    if "params" in meta:
        out["params"] = [_param(p) for p in _list(meta["params"])]
    return out


def _list(value: object) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError("params")
    return value


def _param(spec: object) -> dict[str, Any]:
    if not isinstance(spec, dict):
        raise ValueError("params")
    name, kind, role = spec.get("name"), spec.get("kind"), spec.get("role")
    required = spec.get("required", False)
    if (
        not isinstance(name, str)
        or not name
        or kind not in PARAM_KINDS
        or role not in PARAM_ROLES
        or not isinstance(required, bool)
    ):
        raise ValueError("params")
    return {"name": name, "kind": kind, "role": role, "required": required}


class Model(abc.ABC):
    """One loadable text model and the requests it covers."""

    #: Names the model in ``capabilities`` and in ``warm``/``run``.
    id: str
    task: str
    #: Parameters the model is limited to: ``{"language": ["en"]}``.
    covers: Mapping[str, list[str]]
    #: Free-form parameters it takes whatever they say.
    accepts: list[str]
    #: Typical time for one sentence once warm, in ms (measured).
    latency_ms: int
    #: 0..1, this provider's own estimate; only compared with its other models.
    quality: float
    #: The contract 2 fields its manifest names (see :func:`declared`).
    declared: Mapping[str, Any] = {}

    def capability(self) -> dict[str, Any]:
        """The model as ``capabilities`` lists it.

        ``in_lang`` of a step is ``covers["language"]`` and ``out_lang`` is
        ``covers["to"]``, or ``covers["language"]`` when there is no ``to``.
        """
        cap: dict[str, Any] = {
            "id": self.id,
            "task": self.task,
            "covers": {key: list(values) for key, values in self.covers.items()},
            "accepts": list(self.accepts),
            "latency_ms": self.latency_ms,
            "quality": self.quality,
        }
        default = DEFAULT_CLASS.get(self.task)
        if default is not None:
            cap["class"] = default
        cap.update(self.declared)
        return cap

    def covers_request(self, task: str, params: Params) -> bool:
        """The rule the daemon applies in ``Capability::covers``."""
        if task != self.task:
            return False
        for key, value in params.items():
            allowed = self.covers.get(key)
            if allowed is None:
                if key not in self.accepts:
                    return False
            elif not isinstance(value, str) or value not in allowed:
                return False
        return True

    @abc.abstractmethod
    def warm(self, deadline_s: float) -> None:
        """Load the model and run it once."""

    @abc.abstractmethod
    def run(self, text: str, deadline_s: float) -> str:
        """The model's output for ``text``; raises :class:`Refused` on failure."""

    def close(self) -> None:  # noqa: B027 - most models hold nothing to release
        """Release whatever the model holds."""


class Provider:
    """Answers calls from whatever models ``find_models`` reports.

    ``find_models`` is asked afresh for every ``capabilities`` call, so a model
    component installed while the daemon runs shows up without a restart. A
    model already known is kept, not replaced by the new object, so it stays
    warm.
    """

    def __init__(self, find_models: Callable[[], list[Model]]) -> None:
        self._find = find_models
        self._models: dict[tuple[str, str], Model] = {}

    def _refresh(self) -> None:
        found = {(m.id, m.task): m for m in self._find()}
        kept = {key: self._models.get(key, m) for key, m in found.items()}
        for key, old in self._models.items():
            if key not in kept:
                old.close()
        self._models = kept

    def close(self) -> None:
        for model in self._models.values():
            model.close()
        self._models = {}

    def handle(self, call: object) -> dict[str, Any]:
        started = time.perf_counter()
        op = call.get("op") if isinstance(call, dict) else None
        try:
            reply = self._dispatch(op, call if isinstance(call, dict) else {})
        except Refused as e:
            reply = {"error": str(e)}
        except Exception as e:  # noqa: BLE001 - a call must always be answered
            log.exception("%s failed", op)
            reply = {"error": type(e).__name__}
        outcome = "text" if "text" in reply else reply.get("error", "ok")
        # Never the text: what was asked, of which model, how it ended and when.
        log.info(
            "%s %s -> %s in %.0f ms",
            op,
            _model_of(call),
            outcome,
            (time.perf_counter() - started) * 1000,
        )
        return reply

    def _dispatch(self, op: object, call: dict[str, Any]) -> dict[str, Any]:
        if op == "capabilities":
            self._refresh()
            return {
                "protocol": PROTOCOL,
                "contract": CONTRACT,
                "models": [m.capability() for m in self._models.values()],
            }
        if op not in ("warm", "run"):
            raise Refused("unknown op")
        model = self._models.get((str(call.get("model")), str(call.get("task"))))
        if model is None:
            self._refresh()
            model = self._models.get((str(call.get("model")), str(call.get("task"))))
        if model is None:
            raise Refused("no such model")
        params = call.get("params") or {}
        if not isinstance(params, dict) or not model.covers_request(model.task, params):
            raise Refused("not covered")
        if op == "warm":
            model.warm(_deadline(call, 30.0))
            return {"ok": True}
        text = call.get("text")
        if not isinstance(text, str):
            raise Refused("no text")
        return {"text": model.run(text, _deadline(call, 10.0))}


def _deadline(call: dict[str, Any], default_s: float) -> float:
    ms = call.get("deadline_ms")
    if isinstance(ms, int) and not isinstance(ms, bool) and ms > 0:
        return ms / 1000
    return default_s


def _model_of(call: object) -> str:
    return str(call.get("model")) if isinstance(call, dict) else "-"
