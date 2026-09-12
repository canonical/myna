"""Nemotron adapter metadata + server wiring (no NeMo/model needed).

Constructing the adapter and reading ``.candidate`` does not import NeMo
(that happens lazily in the loader), so this runs in the default offline
suite and guards the candidate labelling, the att_context_size dial, and the
``myna-server --adapter nemotron`` wiring.
"""

import argparse

import pytest

from myna.server.cli import build_adapter, build_parser
from myna.testbed.nemotron import (
    DEFAULT_MODEL,
    NemotronAdapter,
    _parse_att_context_size,
)


def test_candidate_labels_model_engine_strategy():
    cand = NemotronAdapter(DEFAULT_MODEL).candidate
    assert cand.model == "stt_en_fastconformer_hybrid_large_streaming_multi"
    assert cand.engine == "nemo-cuda"
    assert cand.streaming_strategy == "commit-on-finalize"


def test_att_context_size_shows_in_candidate():
    cand = NemotronAdapter(DEFAULT_MODEL, att_context_size=[70, 0]).candidate
    assert cand.model.endswith("-ctx70.0")  # the latency dial is visible in results


def test_local_nemo_checkpoint_labelled_by_stem():
    # snap loads --model $SNAP_COMPONENTS/model-streaming-multi/<file>.nemo
    cand = NemotronAdapter("/snap/nemotron/components/9/model/foo.nemo").candidate
    assert cand.model == "foo"  # no path, no .nemo extension


def test_parse_att_context_size():
    assert _parse_att_context_size("70,16") == [70, 16]
    assert _parse_att_context_size(None) is None
    with pytest.raises(ValueError):
        _parse_att_context_size("70")


def _args(**kw):
    base = dict(
        adapter="whisper",
        model=None,
        device=None,
        compute_type="default",
        att_context_size=None,
        streaming=False,
    )
    base.update(kw)
    return argparse.Namespace(**base)


def test_build_adapter_defaults_per_backend():
    nemo = build_adapter(_args(adapter="nemotron"))
    assert isinstance(nemo, NemotronAdapter)
    assert nemo.candidate.engine == "nemo-cuda"  # nemotron defaults to cuda
    assert nemo.candidate.model.startswith("stt_en_fastconformer")  # default model

    whisper = build_adapter(_args(adapter="whisper"))
    assert whisper.candidate.engine == "faster-whisper-cpu"  # whisper defaults to cpu


def test_parser_accepts_adapter_flag():
    args = build_parser().parse_args(["--socket", "/tmp/s.sock", "--adapter", "nemotron"])
    assert args.adapter == "nemotron"
