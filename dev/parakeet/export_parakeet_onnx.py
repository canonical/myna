#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "nemo_toolkit[asr]==2.7.3",
#     "torch==2.14.0",
#     "onnx==1.22.0",
# ]
#
# [tool.uv.sources]
# torch = { index = "pytorch-cpu" }
#
# [[tool.uv.index]]
# name = "pytorch-cpu"
# url = "https://download.pytorch.org/whl/cpu"
# explicit = true
# ///
"""Export NVIDIA's Parakeet TDT 0.6B v3 checkpoint to the fp32 ONNX graphs
the nvidia-gpu engine serves.

    dev/parakeet/export_parakeet_onnx.py [--cache-dir DIR]

Runs NeMo's own exporter on ``nvidia/parakeet-tdt-0.6b-v3`` at a pinned
revision, sha256-verified, so the only upstream bytes trusted are NVIDIA's
checkpoint and NeMo's code. NeMo and torch live in this script's own uv
environment (the metadata block above, locked beside it), never in
server/uv.lock: they are needed to make the graphs, not to run them.

Writes ``parakeet-tdt-0.6b-v3-fp32`` into the model cache, in the layout
``myna.testbed.parakeet.model_files`` reads: the exporter's encoder (its
weights consolidated into one ``.data`` file) and decoder_joint, plus
``vocab.txt`` from the checkpoint's tokenizer and ``nemo128.onnx``, the mel
preprocessor, from the pinned murmure bundle (NeMo's exporter has no
preprocessor). Checked against NeMo's own frontend on 2026-09-16: interior
frames agree to within 0.006, the first frame differs by up to 0.84 and the
last is counted valid where NeMo pads it.

Measured the same day on the 82-clip balanced corpus: fp32 transcribes
exactly as NeMo's PyTorch model (0.00% WER between them). No fp16 export: the
naive ``onnxruntime.transformers.float16`` conversion measured slower than
fp32 on fresh window lengths and lost accuracy on long windows (5-minute
clip: 5.9% WER vs 0.9%).
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
import tempfile
import urllib.request
from pathlib import Path

REPO = "nvidia/parakeet-tdt-0.6b-v3"
REVISION = "541d1f99c6b0c3cd0b11a95167540bb8edefd82b"
CHECKPOINT = "parakeet-tdt-0.6b-v3.nemo"
SHA256 = "3cbdc85877e668ca7b82d0d56770eb1fac76691f55d6b97545e8d61ca588d10d"
# Bump when the graphs this script writes change for the same checkpoint, so a
# cache made by the old recipe is re-exported rather than staged.
RECIPE = 2
STAMP = f"{REPO}@{REVISION} recipe {RECIPE}"
STAMP_FILE = "UPSTREAM_REVISION"

FP32_FILES = ("encoder-model.onnx", "encoder-model.onnx.data", "decoder_joint-model.onnx")
SHARED_FILES = ("vocab.txt", "nemo128.onnx")

HERE = Path(__file__).resolve().parent


def cache_root() -> Path:
    return Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "myna" / "models"


def staged(out_dir: Path, files: tuple[str, ...]) -> bool:
    stamp = out_dir / STAMP_FILE
    return (
        all((out_dir / f).exists() for f in (*files, *SHARED_FILES))
        and stamp.exists()
        and stamp.read_text(encoding="utf-8").strip() == STAMP
    )


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_checkpoint(root: Path) -> Path:
    """The pinned .nemo, downloaded resumably and verified."""
    path = root / "parakeet-tdt-0.6b-v3-nemo" / CHECKPOINT
    if path.exists() and _sha256(path) == SHA256:
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://huggingface.co/{REPO}/resolve/{REVISION}/{CHECKPOINT}"
    have = path.stat().st_size if path.exists() else 0
    req = urllib.request.Request(url, headers={"Range": f"bytes={have}-"} if have else {})
    with urllib.request.urlopen(req, timeout=60) as resp, path.open("ab") as out:
        while block := resp.read(1 << 20):
            out.write(block)
            print(f"\r{CHECKPOINT}: {(have + out.tell()) / 1e6:.0f} MB", end="", file=sys.stderr)
    print(file=sys.stderr)
    if (digest := _sha256(path)) != SHA256:
        raise RuntimeError(f"sha256 mismatch: {digest} != {SHA256} ({url})")
    return path


def preprocessor(root: Path) -> Path:
    """nemo128.onnx from the pinned murmure bundle the CPU engine ships."""
    sys.path.insert(0, str(HERE))
    import fetch_parakeet_onnx

    return fetch_parakeet_onnx.stage(root / "parakeet-tdt-0.6b-v3-int8") / "nemo128.onnx"


def export_fp32(checkpoint: Path, out_dir: Path) -> None:
    import nemo.collections.asr as nemo_asr
    import onnx

    model = nemo_asr.models.ASRModel.restore_from(str(checkpoint), map_location="cpu")
    model.eval()
    # TMPDIR decides where the per-weight files land: a 2.4 GB detour.
    with tempfile.TemporaryDirectory() as tmp:
        # The encoder is past protobuf's 2 GB limit, so torch writes each
        # weight as its own file; gather them into one beside the graph.
        model.export(str(Path(tmp) / "model.onnx"))
        encoder = onnx.load(str(Path(tmp) / "encoder-model.onnx"))
        onnx.save_model(
            encoder,
            str(out_dir / "encoder-model.onnx"),
            save_as_external_data=True,
            all_tensors_to_one_file=True,
            location="encoder-model.onnx.data",
        )
        # onnx creates the data file owner-only; the rest of the dir is 0644.
        (out_dir / "encoder-model.onnx.data").chmod(0o644)
        joint = "decoder_joint-model.onnx"
        shutil.move(str(Path(tmp) / joint), out_dir / joint)
    with (out_dir / "vocab.txt").open("w", encoding="utf-8") as fh:
        for i, token in enumerate([*model.tokenizer.vocab, "<blk>"]):
            fh.write(f"{token} {i}\n")


def _finish(out_dir: Path, nemo128: Path) -> None:
    shutil.copyfile(nemo128, out_dir / "nemo128.onnx")
    (out_dir / STAMP_FILE).write_text(f"{STAMP}\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cache-dir", type=Path, default=None, help="model cache root")
    root = parser.parse_args().cache_dir or cache_root()
    fp32_dir = root / "parakeet-tdt-0.6b-v3-fp32"
    if staged(fp32_dir, FP32_FILES):
        print(f"already exported at {STAMP}: {fp32_dir}")
        return 0

    nemo128 = preprocessor(root)
    checkpoint = fetch_checkpoint(root)
    shutil.rmtree(fp32_dir, ignore_errors=True)
    fp32_dir.mkdir(parents=True)
    export_fp32(checkpoint, fp32_dir)
    _finish(fp32_dir, nemo128)
    size = sum(f.stat().st_size for f in fp32_dir.iterdir())
    print(f"{fp32_dir.name}: {size / 1e9:.2f} GB")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
