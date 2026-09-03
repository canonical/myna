"""Offline unit test for the sherpa-onnx adapter's thread-pool contract (T65).

Neither the native `_sherpa_onnx` extension nor the staged transducer is
needed: `sherpa_onnx` is stubbed so the assertion is about what the *adapter*
asks for, which is the part that has been wrong twice.
"""

import sys
import types

from myna.testbed.sherpa import DEFAULT_NUM_THREADS, SherpaAdapter


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
