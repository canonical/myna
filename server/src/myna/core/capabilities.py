"""Service capabilities — what a backend can do, for client discovery (T24).

A static description the client queries *before* opening a session: which model
and input languages the service serves, what audio format(s) it accepts, and
whether it does native punctuation/capitalisation or translation. This is the
discovery surface IE114 comment [ac] asked for.

``input_formats`` is also the audio-format advertisement: under the audio-push
model the client owns capture and conversion (PipeWire), so the service states
what it accepts and the client delivers exactly that — the service never
resamples (see ``myna.core.audio`` and the adapters, which reject off-format
audio rather than converting it).

Model *selection* is out of band (the IE108/modelctl CLI per IE114's
Configuration API); a running server serves one model, so ``models`` reports
the active one rather than everything installable.

Transport-agnostic, like ``SessionConfig`` and the event vocabulary: a plain
JSON object served over whatever transport the deployment uses. Over the
WebSocket transport it answers a ``capabilities.query`` message; the loopback
client calls the service method directly.

PROVISIONAL — input to the IE114 capabilities-discovery API (T24).
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any

from myna.core.audio import AudioFormat


@dataclass(frozen=True)
class Capabilities:
    """What a backend can do.

    - ``models``: the active model id(s) the server serves (informational).
    - ``languages``: BCP-47-ish input language tags, or ``("*",)`` for "any
      language the multilingual model handles".
    - ``input_formats``: the PCM formats the service accepts; the client must
      deliver one of them.
    - ``punctuation``: the model emits punctuation/capitalisation natively.
    - ``translation``: the service can output a language different from the
      input (IE114 ``output-language``).
    """

    models: tuple[str, ...] = ()
    languages: tuple[str, ...] = ("*",)
    input_formats: tuple[AudioFormat, ...] = (AudioFormat(),)
    punctuation: bool = False
    translation: bool = False


def capabilities_to_wire(caps: Capabilities) -> dict[str, Any]:
    return asdict(caps)


def capabilities_from_wire(wire: dict[str, Any]) -> Capabilities:
    data = dict(wire)
    fmts = data.get("input_formats")
    if fmts is not None:
        data["input_formats"] = tuple(AudioFormat(**f) for f in fmts)
    for key in ("models", "languages"):  # JSON arrays -> tuples
        if data.get(key) is not None:
            data[key] = tuple(data[key])
    return Capabilities(**data)
