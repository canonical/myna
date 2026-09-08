"""Offline unit test for the sherpa-onnx adapter's thread-pool contract (T65).

Neither the native `_sherpa_onnx` extension nor the staged transducer is
needed: `sherpa_onnx` is stubbed so the assertion is about what the *adapter*
asks for, which is the part that has been wrong twice.
"""

import sys
import types

import pytest

from myna.testbed.sherpa import (
    _PUNCT_FILES,
    DEFAULT_NUM_THREADS,
    SherpaAdapter,
    _punct_dir_if_complete,
)


async def test_load_caps_the_intra_op_pool_at_the_measured_width(monkeypatch, tmp_path):
    """A small pool, not the machine's width, and not ORT's own sizing.

    sherpa-onnx forwards `num_threads` straight to `intra_op_num_threads`, so
    three machine-wide pools contend over 480 ms chunks whose tensors are far
    too small to divide. Measured 2026-09-03 over 1020 s of the English corpus
    (RTF): 2 -> 0.0372 against 16 -> 0.1834 and 0 -> 0.1342, so the width the
    adapter briefly shipped cost 4.9x. Guarded here rather than in
    test_snap_packaging.py's AST walk, which only sees `intra_op_num_threads`
    and cannot see a default argument being widened.
    """
    captured: dict = {}

    class _Recognizer:
        @staticmethod
        def from_transducer(*args, **kwargs):
            captured.update(kwargs)
            return object()

    module = types.ModuleType("sherpa_onnx")
    module.OnlineRecognizer = _Recognizer
    monkeypatch.setitem(sys.modules, "sherpa_onnx", module)

    await SherpaAdapter(str(tmp_path))._load_model()

    assert captured["num_threads"] == DEFAULT_NUM_THREADS == 2


# --- punctuation restoration (2026-09-08) -----------------------------------
#
# The transducer's vocabulary is 1025 tokens whose only punctuation is an
# apostrophe, so committed text is punctuated by a second model or not at all.
# Asserted here: how the model is resolved, what capabilities then promise, and
# that a failure in a cosmetic pass never costs the utterance. *Where* it runs
# in the push loop is a routing question, and lives with the other routing
# invariants in tests/testbed/test_sherpa.py.


def _staged_punct(tmp_path):
    """A punctuation model laid out the way the fetcher leaves it."""
    d = tmp_path / "punct"
    d.mkdir()
    for name in _PUNCT_FILES:
        (d / name).write_bytes(b"")
    return d


class _StubPunct:
    """Stand-in for ``OnlinePunctuation``: records every text it is handed."""

    def __init__(self, *_args, **_kwargs):
        self.seen = []

    def add_punctuation_with_case(self, text):
        self.seen.append(text)
        return f"<{text}>"


def _install_punct(monkeypatch, adapter, tmp_path):
    """Give ``adapter`` a stub punctuation model and return it."""
    punct = _StubPunct()
    adapter._punct_dir = str(_staged_punct(tmp_path))
    adapter._punct = punct
    return punct


# --- resolution + capabilities ----------------------------------------------


def test_capabilities_promise_punctuation_only_when_the_model_is_staged(monkeypatch, tmp_path):
    """`punctuation` describes the service, not the weights.

    Advertising a constant true would have the client skip its own
    post-processing on a checkout that staged only the transducer, and the user
    would get lowercase text with nothing having said so.
    """
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "empty"))
    assert SherpaAdapter().capabilities().punctuation is False

    staged = _staged_punct(tmp_path)
    assert SherpaAdapter(punct_dir=str(staged)).capabilities().punctuation is True


def test_punctuate_false_declines_a_staged_model(tmp_path):
    adapter = SherpaAdapter(punct_dir=str(_staged_punct(tmp_path)), punctuate=False)
    assert adapter.punctuates is False
    assert adapter.capabilities().punctuation is False


def test_explicit_incomplete_punct_dir_raises_rather_than_degrading(tmp_path):
    """An operator naming a path is asking for punctuation by name.

    Falling back silently there ships a snap whose transcripts are lowercase
    for a reason nothing states - the failure mode this whole flag exists to
    avoid.
    """
    half = tmp_path / "half"
    half.mkdir()
    (half / _PUNCT_FILES[0]).write_bytes(b"")
    with pytest.raises(FileNotFoundError, match="punctuation model incomplete"):
        SherpaAdapter(punct_dir=str(half))


def test_punct_dir_resolution_needs_every_artifact(tmp_path):
    assert _punct_dir_if_complete(None) is None
    assert _punct_dir_if_complete(tmp_path / "nope") is None
    assert _punct_dir_if_complete(_staged_punct(tmp_path)) is not None


# --- failure posture --------------------------------------------------------


def test_punctuation_failure_degrades_to_raw_text(monkeypatch, tmp_path):
    """A cosmetic pass must never cost the utterance."""

    class _Boom(_StubPunct):
        def add_punctuation_with_case(self, text):
            raise RuntimeError("bad graph")

    adapter = SherpaAdapter(str(tmp_path), punctuate=False)
    adapter._punct = _Boom()
    assert adapter._punctuate("hello there") == "hello there"


def test_punctuation_of_empty_text_never_reaches_the_model(monkeypatch, tmp_path):
    adapter = SherpaAdapter(str(tmp_path), punctuate=False)
    punct = _install_punct(monkeypatch, adapter, tmp_path)
    assert adapter._punctuate("") == ""
    assert punct.seen == []


async def test_unload_releases_the_punctuation_model_too(tmp_path):
    """52 MB resident, on the same idle path as the recognizer (T27)."""
    adapter = SherpaAdapter(str(tmp_path), punctuate=False)
    adapter._recognizer, adapter._punct = object(), _StubPunct()
    await adapter.unload()
    assert adapter._recognizer is None
    assert adapter._punct is None
    await adapter.unload()  # idempotent
