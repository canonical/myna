"""Candidate-adapter evaluation testbed.

Purpose: produce the reference-hardware tiers IE114/UD129 assume but do not
define, via a matrix sweep of model x hardware x streaming strategy.

Rules of the house (from CLAUDE.md):

- The harness speaks only the IE114-shaped interfaces in ``myna.core``.
  Never modify the harness to accommodate a candidate — fix the adapter.
- The fake adapter is a permanent regression fixture, not throwaway code.
"""

from myna.testbed.adapter import Adapter, Candidate
from myna.testbed.corpus import (
    Clip,
    by_category,
    corpus_id,
    digest_files,
    load_manifest,
    stamp_corpus,
    verify_corpus,
)
from myna.testbed.fake import FakeAdapter, ScriptStep
from myna.testbed.harness import (
    DecodeSample,
    Harness,
    Metrics,
    ResultRecord,
    StreamingTelemetry,
    TimedEvent,
)
from myna.testbed.metrics import (
    ErrorRate,
    character_error_rate,
    normalize,
    word_error_rate,
)
from myna.testbed.sources import SilenceSource, WavFileSource

__all__ = [
    "Adapter",
    "Candidate",
    "Clip",
    "DecodeSample",
    "ErrorRate",
    "FakeAdapter",
    "Harness",
    "Metrics",
    "ResultRecord",
    "ScriptStep",
    "SilenceSource",
    "StreamingTelemetry",
    "TimedEvent",
    "WavFileSource",
    "by_category",
    "character_error_rate",
    "corpus_id",
    "digest_files",
    "load_manifest",
    "normalize",
    "stamp_corpus",
    "verify_corpus",
    "word_error_rate",
]
