#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "nemo_toolkit[asr]==3.0.0",
#     "torch==2.14.0",
#     "onnx==1.23.2",
#     "onnxruntime==1.27.0",
#     # myna.core, which the adapter imports, needs it.
#     "websockets==16.0",
# ]
#
# [tool.uv]
# # lightning 2.4.0, the newest NeMo allows, caps fsspec below 2026, and every
# # fsspec before 2026.6.0 renders ReferenceFileSystem templates unsafely.
# override-dependencies = ["fsspec[http]>=2026.6.0"]
#
# [tool.uv.sources]
# torch = { index = "pytorch-cpu" }
#
# [[tool.uv.index]]
# name = "pytorch-cpu"
# url = "https://download.pytorch.org/whl/cpu"
# explicit = true
# ///
"""Check an IndicConformer export, served by Myna's adapter, against NeMo.

    parity.py {large,600m} LANGUAGE MANIFEST [--clips N] [--int8]

Cuts the language out of the checkpoint as the export does, transcribes the
manifest's clips with NeMo (RNN-T and CTC, greedy, dither off), then with
``myna.testbed.indicconformer`` over the exported graphs, whole clips, and
prints per decoder how many transcripts are identical and the WER between
the two. fp32 unless ``--int8``. On 2026-11-17 the fp32 large export was
identical on all 418 FLEURS hi_in test clips with either decoder.
"""

import argparse
import json
import sys
import tarfile
import tempfile
import wave
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path[:0] = [str(HERE), str(HERE.parents[1] / "server" / "src")]

import export_indicconformer_onnx as export  # noqa: E402

from myna.testbed.indicconformer import _Model  # noqa: E402
from myna.testbed.metrics import normalize, word_error_rate  # noqa: E402

parser = argparse.ArgumentParser()
parser.add_argument("model", choices=sorted(export.MODELS))
parser.add_argument("language")
parser.add_argument("manifest", type=Path)
parser.add_argument("--clips", type=int, default=0, help="the first N clips only")
parser.add_argument("--int8", action="store_true")
args = parser.parse_args()

clips = json.loads(args.manifest.read_text(encoding="utf-8"))["clips"]
clips = clips[: args.clips] if args.clips else clips
wavs = [str(args.manifest.parent / c["path"]) for c in clips]
root = export.cache_root()
graphs = root / f"indicconformer-{args.model}-{args.language}"

with tempfile.TemporaryDirectory(dir=root) as tmp:
    with tarfile.open(export.fetch_checkpoint(export.MODELS[args.model], root)) as tar:
        tar.extractall(tmp, filter="data")
    model = export.monolingual(Path(tmp), args.language)
nemo = {}
for decoder in ("rnnt", "ctc"):
    if decoder == "ctc":
        model.change_decoding_strategy(decoder_type="ctc", verbose=False)
    hyps = model.transcribe(wavs, batch_size=4, verbose=False)
    nemo[decoder] = [h.text if hasattr(h, "text") else h for h in hyps]
del model

with tempfile.TemporaryDirectory() as tmp:
    served = Path(tmp) / graphs.name
    served.mkdir()
    for f in graphs.iterdir():
        if args.int8 or ".int8." not in f.name:
            (served / f.name).symlink_to(f)
    for decoder in ("rnnt", "ctc"):
        adapter = _Model(served, decoder, 4)
        same = errors = words = 0
        for wav, reference in zip(wavs, nemo[decoder], strict=True):
            with wave.open(wav) as w:
                pcm = w.readframes(w.getnframes())
            samples = np.frombuffer(pcm, "<i2").astype(np.float32) / 32768.0
            text = "".join(w.text for w in adapter.transcribe_words(samples)).strip()
            same += normalize(text) == normalize(reference)
            er = word_error_rate(reference, text)
            errors += er.substitutions + er.deletions + er.insertions
            words += er.reference_length
        print(
            f"{decoder} {adapter.precision}: {same}/{len(wavs)} identical, "
            f"WER against NeMo {100 * errors / max(words, 1):.2f}%"
        )
