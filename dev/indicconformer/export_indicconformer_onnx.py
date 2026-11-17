#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "nemo_toolkit[asr]==3.0.0",
#     "torch==2.14.0",
#     "onnx==1.23.2",
#     "onnxruntime==1.27.0",
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
"""Export one language of an AI4Bharat IndicConformer checkpoint to the ONNX
graphs Myna's ``indicconformer`` adapter serves.

    dev/indicconformer/export_indicconformer_onnx.py {large,600m} LANGUAGE

The checkpoints are multilingual hybrid RNN-T/CTC models trained with
AI4Bharat's NeMo fork: one encoder, one prediction net, and per language a
256-token SentencePiece vocabulary, its own joint output layer and its slice
of the CTC head (``multisoftmax``). Stock NeMo cannot load them, and needs
none of that to serve one language, so this cuts the language out into a
plain monolingual hybrid model and runs NeMo's own exporter on it:

- the joint's final layer is ``joint_net.2.<lang>`` (256 tokens and blank);
- the CTC head keeps the language's rows of the aggregate vocabulary and the
  blank (the fork's ``language_masks``);
- the prediction net is fed the language-local token id: the fork's greedy
  decoder feeds back the joint's argmax, so only the embedding's first 256
  rows (and the blank pad) are ever read. Embedding rows at the language's
  aggregate offset instead decode garbage, checked on FLEURS hi_in.

Fetches the checkpoint from AI4Bharat's object store, sha256-verified, and
writes ``indicconformer-<model>-<lang>`` into the model cache
(``$XDG_CACHE_HOME/myna/models``), stamped with what it was made from:

- ``encoder-model.onnx``, ``decoder_joint-model.onnx``: NeMo's exporter, fp32
  (weights in one ``.data`` file each), and their ``.int8.onnx`` twins
  (dynamic; the encoder's MatMul weights only, as for Nemotron).
- ``ctc-model.onnx``: the CTC head, encoder frames to log-probabilities.
- ``mel-model.onnx``: the checkpoint's mel filterbank and log, power
  spectrum to log-mel. The adapter frames and transforms in numpy (torch
  cannot export NeMo's STFT) and normalises there too; the window is a
  symmetric Hann, checked against the checkpoint's.
- ``vocab.txt``, ``config.json``: tokens (blank last) and the preprocessor.
"""

from __future__ import annotations

import argparse
import gc
import hashlib
import json
import os
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass(frozen=True)
class Checkpoint:
    url: str
    sha256: str


BASE = "https://objectstore.e2enetworks.net/indicconformer/models"
# The per-language "large" downloads AI4Bharat lists are this one 22-language
# checkpoint under each name (the hi file was checked); 600m is the
# multilingual model their Hugging Face card describes.
MODELS = {
    "large": Checkpoint(
        url=f"{BASE}/indicconformer_stt_hi_hybrid_rnnt_large.nemo",
        sha256="7cad1308751a56aed1fda1f30c9078077b1906a2e04454f92128d327561d9008",
    ),
    "600m": Checkpoint(
        url=f"{BASE}/indicconformer_stt_multi_hybrid_rnnt_600m.nemo",
        sha256="5d33cd4c21b5ec60c51e292b9d8af065b67ad1e80135266a0546ec5c60148426",
    ),
}
# Bump when the graphs this script writes change for the same checkpoint, so a
# cache made by the old recipe is re-exported rather than staged.
RECIPE = 2
STAMP_FILE = "UPSTREAM_REVISION"


def stamp(model: str, language: str) -> str:
    c = MODELS[model]
    return f"{c.url} sha256:{c.sha256} {language} recipe {RECIPE}"


def cache_root() -> Path:
    return Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "myna" / "models"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_checkpoint(c: Checkpoint, root: Path) -> Path:
    """The pinned .nemo, downloaded resumably and verified."""
    path = root / "indicconformer-src" / c.url.rsplit("/", 1)[1]
    if path.exists() and _sha256(path) == c.sha256:
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    have = path.stat().st_size if path.exists() else 0
    req = urllib.request.Request(c.url, headers={"Range": f"bytes={have}-"} if have else {})
    with urllib.request.urlopen(req, timeout=60) as resp, path.open("ab") as out:
        while block := resp.read(1 << 20):
            out.write(block)
            print(f"\r{path.name}: {(have + out.tell()) / 1e6:.0f} MB", end="", file=sys.stderr)
    print(file=sys.stderr)
    if (digest := _sha256(path)) != c.sha256:
        raise RuntimeError(f"sha256 mismatch: {digest} != {c.sha256} ({c.url})")
    return path


def _member(src: Path, ref: str) -> Path:
    """A ``nemo:<file>`` config reference, resolved in the extracted archive."""
    return src / ref.removeprefix("nemo:")


def monolingual(src: Path, language: str) -> Any:
    """A stock NeMo hybrid model of ``language`` alone, from the extracted
    multisoftmax checkpoint ``src``."""
    import sentencepiece as spm
    import torch
    import yaml
    from nemo.collections.asr.models import EncDecHybridRNNTCTCBPEModel
    from omegaconf import OmegaConf

    cfg = yaml.safe_load((src / "model_config.yaml").read_text(encoding="utf-8"))
    weights = torch.load(src / "model_weights.ckpt", map_location="cpu")
    langs = cfg["tokenizer"]["langs"]
    if language not in langs:
        raise SystemExit(f"{language} is not one of {sorted(langs)}")
    # The aggregate vocabulary concatenates the languages in config order.
    sizes = {
        name: spm.SentencePieceProcessor(
            model_file=str(_member(src, t["model_path"]))
        ).get_piece_size()
        for name, t in langs.items()
    }
    names = list(langs)
    offset = sum(sizes[n] for n in names[: names.index(language)])
    size = sizes[language]
    total = cfg["decoder"]["vocab_size"]
    tok = langs[language]
    vocab = [
        line.split("\t")[0]
        for line in _member(src, tok["spe_tokenizer_vocab"])
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert len(vocab) == size, (len(vocab), size)

    for split in ("train_ds", "test_ds"):
        cfg.pop(split, None)
    # transcribe() reads validation_ds's shape; no data behind it.
    cfg["validation_ds"] = {
        "manifest_filepath": None,
        "sample_rate": 16000,
        "batch_size": 4,
        "shuffle": False,
    }
    cfg["tokenizer"] = {
        "dir": str(src),
        "type": "bpe",
        "model_path": str(_member(src, tok["model_path"])),
        "vocab_path": str(_member(src, tok["vocab_path"])),
        "spe_tokenizer_vocab": str(_member(src, tok["spe_tokenizer_vocab"])),
    }
    cfg["decoder"].pop("multisoftmax")
    cfg["decoder"]["vocab_size"] = size
    cfg["joint"].pop("multilingual")
    cfg["joint"].pop("language_keys")
    cfg["joint"]["num_classes"] = size
    cfg["joint"]["vocabulary"] = vocab
    cfg["aux_ctc"]["decoder"].pop("multisoftmax", None)
    cfg["aux_ctc"]["decoder"]["num_classes"] = size
    cfg["aux_ctc"]["decoder"]["vocabulary"] = vocab
    cfg["preprocessor"]["dither"] = 0.0

    rows = [*range(offset, offset + size), total]
    sliced = {}
    head = "joint.joint_net.2."
    for key, value in weights.items():
        if key.startswith(head):
            if key.startswith(f"{head}{language}."):
                sliced[head + key.rsplit(".", 1)[1]] = value
        elif key.startswith("ctc_decoder.decoder_layers.0."):
            sliced[key] = value[rows]
        elif key == "decoder.prediction.embed.weight":
            sliced[key] = value[[*range(size), total]]
        else:
            sliced[key] = value
    model = EncDecHybridRNNTCTCBPEModel(cfg=OmegaConf.create(cfg))
    model.load_state_dict(sliced, strict=True)
    return model.eval()


def export(model: Any, out: Path, source: str) -> None:
    import numpy as np
    import onnx
    import onnxruntime as ort
    import torch

    tempfile.tempdir = str(out)
    raw = out / "raw"
    raw.mkdir(exist_ok=True)
    t0 = time.time()
    model.export(str(raw / "model.onnx"))
    # The 600m encoder is past protobuf's 2 GB limit: NeMo's exporter then
    # scatters its weights one file per tensor. Gather each graph's into one
    # beside it, whatever its size, so both models share a layout.
    for part in ("encoder", "decoder_joint"):
        name = f"{part}-model.onnx"
        onnx.save_model(
            onnx.load(str(raw / name)),
            str(out / name),
            save_as_external_data=True,
            all_tensors_to_one_file=True,
            location=f"{name}.data",
        )
    shutil.rmtree(raw)
    print("exported in", f"{time.time() - t0:.0f}s", flush=True)

    class CtcHead(torch.nn.Module):
        """Encoder frames (B, D, T) to log-probabilities (B, T, V + 1)."""

        def __init__(self, conv: torch.nn.Module) -> None:
            super().__init__()
            self.conv = conv

        def forward(self, encoded: torch.Tensor) -> torch.Tensor:
            return torch.log_softmax(self.conv(encoded).transpose(1, 2), dim=-1)

    head = CtcHead(model.ctc_decoder.decoder_layers[0]).eval()
    enc = torch.randn(1, model.cfg.encoder.d_model, 9)
    torch.onnx.export(
        head,
        (enc,),
        str(out / "ctc-model.onnx"),
        input_names=["encoder_outputs"],
        output_names=["logprobs"],
        dynamic_axes={"encoder_outputs": {0: "B", 2: "T"}, "logprobs": {0: "B", 1: "T"}},
        opset_version=17,
        dynamo=False,
    )
    got = ort.InferenceSession(str(out / "ctc-model.onnx")).run(
        None, {"encoder_outputs": enc.numpy()}
    )[0]
    with torch.no_grad():
        diff = float(abs(got - head(enc).numpy()).max())
    if diff > 1e-4:
        raise RuntimeError(f"CTC graph differs from torch by {diff}")

    feat = model.preprocessor.featurizer
    window = np.hanning(feat.win_length)
    if np.abs(window - feat.window.numpy()).max() > 1e-6:
        raise RuntimeError("the checkpoint's window is not a symmetric Hann")

    class LogMel(torch.nn.Module):
        """Power spectrum (B, n_fft / 2 + 1, T) to log-mel (B, features, T)."""

        def __init__(self, fb: torch.Tensor, guard: float) -> None:
            super().__init__()
            self.register_buffer("fb", fb)
            self.guard = guard

        def forward(self, power: torch.Tensor) -> torch.Tensor:
            return torch.log(torch.matmul(self.fb, power) + self.guard)

    logmel = LogMel(feat.fb[0].clone(), float(feat.log_zero_guard_value)).eval()
    power = torch.rand(1, feat.fb.shape[-1], 11)
    torch.onnx.export(
        logmel,
        (power,),
        str(out / "mel-model.onnx"),
        input_names=["power"],
        output_names=["logmel"],
        dynamic_axes={"power": {0: "B", 2: "T"}, "logmel": {0: "B", 2: "T"}},
        opset_version=17,
        dynamo=False,
    )
    with (out / "vocab.txt").open("w", encoding="utf-8") as fh:
        for i, token in enumerate([*model.tokenizer.vocab, "<blk>"]):
            fh.write(f"{token} {i}\n")
    pre = dict(model.cfg.preprocessor)
    (out / "config.json").write_text(
        json.dumps(
            {
                "source": source,
                "preprocessor": {
                    "features": pre["features"],
                    "n_fft": pre["n_fft"],
                    "hop": feat.hop_length,
                    "win_length": feat.win_length,
                    "preemph": feat.preemph,
                    "normalize": pre["normalize"],
                },
                "subsampling_factor": model.encoder.subsampling_factor,
                "max_symbols": 10,
            },
            indent=1,
        )
    )


def quantize(out: Path) -> None:
    """int8 twins: the encoder's MatMuls only (quantising Nemotron's Conv
    layers too ran slower, 2026-11-02), the decoder_joint whole. Run once
    the torch model is gone, or the 600m export does not fit in 10 GB."""
    from onnxruntime.quantization import QuantType, quantize_dynamic

    for part in ("encoder", "decoder_joint"):
        t0 = time.time()
        quantize_dynamic(
            out / f"{part}-model.onnx",
            out / f"{part}-model.int8.onnx",
            weight_type=QuantType.QInt8,
            op_types_to_quantize=["MatMul"] if part == "encoder" else None,
        )
        print("quantised", part, f"{time.time() - t0:.0f}s", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("model", choices=sorted(MODELS))
    parser.add_argument("language")
    parser.add_argument("--save-nemo", type=Path, help="also save the monolingual .nemo here")
    args = parser.parse_args()

    root = cache_root()
    out = root / f"indicconformer-{args.model}-{args.language}"
    want = stamp(args.model, args.language)
    marker = out / STAMP_FILE
    if marker.exists() and marker.read_text(encoding="utf-8").strip() == want:
        print(f"{out} is already {want}")
        return 0
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    checkpoint = fetch_checkpoint(MODELS[args.model], root)
    with tempfile.TemporaryDirectory(dir=out) as tmp:
        with tarfile.open(checkpoint) as tar:
            tar.extractall(tmp, filter="data")
        model = monolingual(Path(tmp), args.language)
        if args.save_nemo:
            model.save_to(str(args.save_nemo))
        export(model, out, checkpoint.name)
        del model
    gc.collect()
    quantize(out)
    marker.write_text(f"{want}\n", encoding="utf-8")
    print(f"{out}: {want}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
