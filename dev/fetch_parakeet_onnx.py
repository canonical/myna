"""Stage the Parakeet TDT 0.6B v3 int8 ONNX export into the model cache (008/T023).

    uv run python dev/fetch_parakeet_onnx.py [--out-dir DIR]

Source: `istupakov/parakeet-tdt-0.6b-v3-onnx` — the ONNX export murmure ships
(same file layout as murmure's bundled `parakeet-tdt-0.6b-v3-int8` resources):
`nemo128.onnx` (mel preprocessor), `encoder-model.int8.onnx`,
`decoder_joint-model.int8.onnx` (TDT decoder+joint), `vocab.txt`. We fetch only
the int8 weights — the fp32 encoder/decoder are the snap's opt-in variants and
are several GB.

Model: NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0), 25 languages, punctuation +
capitalisation. ONNX export layout by istupakov (parakeet.cpp).

Download goes through `huggingface_hub.snapshot_download` (resumable, cached
under HF_HOME like the whisper models). Verify offline afterwards with:

    HF_HUB_OFFLINE=1 uv run python dev/fetch_parakeet_onnx.py
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO_ID = "istupakov/parakeet-tdt-0.6b-v3-onnx"
# int8 weights + preprocessor + vocab only; the fp32 encoder/decoder
# (encoder-model.onnx + .onnx.data) are multi-GB and unused by the adapter.
FILES = (
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="download here instead of the HF cache (e.g. a lab cache to pass "
        "as --model to myna-server --adapter parakeet)",
    )
    args = parser.parse_args()

    try:
        from huggingface_hub import snapshot_download
    except ImportError:
        sys.exit("huggingface_hub is required (uv sync --extra whisper provides it)")

    path = Path(
        snapshot_download(
            REPO_ID,
            allow_patterns=list(FILES),
            local_dir=str(args.out_dir) if args.out_dir else None,
        )
    )
    missing = [f for f in FILES if not (path / f).exists()]
    if missing:
        sys.exit(f"download incomplete, missing: {', '.join(missing)}")

    total = sum((path / f).stat().st_size for f in FILES)
    print(f"parakeet int8 ONNX ready: {path} ({total / 1e6:.0f} MB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
