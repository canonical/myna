"""Candidate-adapter evaluation testbed.

Purpose: produce the reference-hardware tiers IE114/UD129 assume but do not
define, via a matrix sweep of model x hardware x streaming strategy.

Rules of the house (from CLAUDE.md):

- The harness speaks only the IE114-shaped interfaces in ``myna.core``.
  Never modify the harness to accommodate a candidate — fix the adapter.
- The fake adapter is a permanent regression fixture, not throwaway code.
"""

from myna.testbed.adapter import Adapter, Candidate
from myna.testbed.fake import FakeAdapter, ScriptStep
from myna.testbed.harness import Harness, Metrics, ResultRecord, TimedEvent
from myna.testbed.sources import SilenceSource

__all__ = [
    "Adapter",
    "Candidate",
    "FakeAdapter",
    "Harness",
    "Metrics",
    "ResultRecord",
    "ScriptStep",
    "SilenceSource",
    "TimedEvent",
]
