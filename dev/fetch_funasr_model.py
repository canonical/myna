"""Fetch SenseVoice-Small ONNX model from ModelScope (009 US1).

    uv run python dev/fetch_funasr_model.py [--target DIR]

Downloads the ONNX-exported SenseVoice-Small model from ModelScope
(botaruibo/SenseVoiceSmall-onnx, v1.0) into ``--target`` (default:
``$HF_HOME/hub/models--botaruibo--SenseVoiceSmall-onnx/snapshots/<hash>/``
or a flat local dir). The ONNX export lives on ModelScope only —
there is no Hugging Face mirror for this artifact.

Files downloaded::

    model.onnx          # CTC graph weights (fp32, ~937 MB)
    model_quant.onnx    # int8 quantized (--quantize flag, ~234 MB)
    config.yaml         # fbank frontend config
    am.mvn              # global CMVN statistics
    chn_jpn_yue_eng_ko_spectok.bpe.model  # SentencePiece tokenizer

After first download, the target is cache-hit (offline-capable if staged).
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

MODELSCOPE_REPO = "botaruibo/SenseVoiceSmall-onnx"
REVISION = "v1.0"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--target", type=Path, default=None, help="Output directory (default: ./funasr_model/)"
    )
    args = parser.parse_args()

    # One-time import: modelscope is NOT a server runtime dep — this script is
    # a dev/build-time tool only (mirrors dev/fetch_sherpa_model.py).
    try:
        from modelscope.hub.snapshot_download import snapshot_download  # type: ignore
    except ImportError:
        print("modelscope not installed. Installing...", file=sys.stderr)
        import subprocess

        # Use uv if we're inside its venv (no pip module), pip otherwise.
        package = "modelscope>=1.34"
        installer = ["uv", "pip", "install", "--quiet", package]
        try:
            subprocess.check_call(installer, stdout=sys.stderr)
        except (FileNotFoundError, subprocess.CalledProcessError):
            pip = [sys.executable, "-m", "pip", "install", "--break-system-packages", package]
            subprocess.check_call(pip, stdout=sys.stderr)
        from modelscope.hub.snapshot_download import snapshot_download  # type: ignore

    print(f"⬇️  Downloading {MODELSCOPE_REPO}@{REVISION} from ModelScope...")
    snapshot_dir = snapshot_download(
        MODELSCOPE_REPO,
        revision=REVISION,
        cache_dir=os.environ.get("HF_HOME"),
    )
    print(f"   Cached at: {snapshot_dir}")

    target = args.target
    if target is None:
        target = Path.cwd() / "funasr_model"

    target.mkdir(parents=True, exist_ok=True)

    # Copy the four essential files into a flat target directory
    # (SenseVoiceSmall(model_dir=...) expects them flat; ModelScope
    # sometimes nests them in a subdirectory).
    # The four essential files. The repo may ship only model_quant.onnx (int8)
    # without model.onnx (fp32). Accept whichever ONNX file is present.
    required_files = (
        "config.yaml",
        "am.mvn",
        "chn_jpn_yue_eng_ko_spectok.bpe.model",
    )
    onnx_candidates = ("model.onnx", "model_quant.onnx")

    snapshot_path = Path(snapshot_dir)
    # Walk the snapshot tree to find each file (ModelScope may nest one level deep)
    for filename in required_files:
        found = None
        for candidate in snapshot_path.rglob(filename):
            found = candidate
            break
        if found is None:
            print(f"❌ Missing required file: {filename}", file=sys.stderr)
            return 1
        dst = target / filename
        if not dst.exists() or dst.stat().st_size != found.stat().st_size:
            shutil.copy2(found, dst)
            print(f"   {filename} -> {target}")

    # Find at least one ONNX model file
    onnx_found = None
    onnx_name = None
    for name in onnx_candidates:
        for candidate in snapshot_path.rglob(name):
            onnx_found = candidate
            onnx_name = name
            break
        if onnx_found:
            break
    if onnx_found is None:
        print("❌ No ONNX model file found (model.onnx or model_quant.onnx)", file=sys.stderr)
        return 1
    dst = target / onnx_name
    if not dst.exists() or dst.stat().st_size != onnx_found.stat().st_size:
        shutil.copy2(onnx_found, dst)
        print(f"   {onnx_name} -> {target}")

    # Handle quantized variant — just log, don't try to copy from snapshot
    # (already handled by the onnx_candidates loop above)
    quant_path = target / "model_quant.onnx"
    if quant_path.exists():
        print(
            f"   model_quant.onnx present (int8, ~{quant_path.stat().st_size // (1024 * 1024)}MB)"
        )
    fp32_path = target / "model.onnx"
    if fp32_path.exists():
        print(f"   model.onnx present (fp32, ~{fp32_path.stat().st_size // (1024 * 1024)}MB)")

    print(f"✅ FunASR model staged to {target}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
