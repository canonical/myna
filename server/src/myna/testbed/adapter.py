"""Adapter and candidate definitions.

A *candidate* is a specific (model, engine, streaming strategy) combination
under evaluation. An *adapter* wraps one candidate behind the IE114-shaped
``SttService`` interface so the harness can drive it without knowing anything
model-specific.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Protocol, runtime_checkable

from myna.core import EventSink, PcmChunk, SessionConfig, SttService


@dataclass(frozen=True)
class Candidate:
    """One point in the evaluation matrix.

    streaming_strategy examples:
    - "commit-on-finalize" — single final at end of audio (Phase 2)
    - "chunked-redecode"   — AED bolt-on streaming, e.g. LocalAgreement (Phase 3)
    - "native-transducer"  — natively streaming, e.g. Nemotron RNNT (Phase 3+)
    - "scripted"           — fake adapter, no model
    """

    model: str
    engine: str
    streaming_strategy: str

    @property
    def id(self) -> str:
        return f"{self.model}/{self.engine}/{self.streaming_strategy}"


@runtime_checkable
class Adapter(SttService, Protocol):
    """An ``SttService`` that also identifies which candidate it wraps."""

    @property
    def candidate(self) -> Candidate: ...

    async def run_session(
        self,
        config: SessionConfig,
        audio: AsyncIterator[PcmChunk],
        emit: EventSink,
    ) -> None: ...
