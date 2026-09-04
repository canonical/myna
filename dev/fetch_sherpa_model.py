"""Stage the sherpa-onnx streaming FastConformer transducer (008 US4 / T029).

    uv run python dev/fetch_sherpa_model.py [--fix-libs]

Model: `csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8`
— a NeMo streaming FastConformer transducer exported to ONNX by k2-fsa
(int8, encoder/decoder/joiner + tokens.txt), pinned to REVISION below. Using
the pre-exported model collapses research.md Decision 8's k2 export step into
a fetch; the 80ms and 1040ms latency variants and the Zipformer fallback
download the same way (pass --repo, which resolves that repo's own main).

--fix-libs: sherpa-onnx's native module needs onnxruntime 1.27.x's
libonnxruntime but the wheel doesn't bundle it — symlink the pip package's
lib into sherpa_onnx.libs (on its RPATH). Needed once per venv rebuild; the
snap bundles its own runtime instead.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

REPO_ID = "csukuangfj/sherpa-onnx-nemo-streaming-fast-conformer-transducer-en-480ms-int8"
# Pinned upstream revision. Unpinned, snapshot_download resolves the repo's
# current main at fetch time, so the same command months apart can stage
# different weights - see dev/model-pin.sh. sherpa-snap/dev/download-models.sh
# pins the same commit for the packed component; test_model_pins.py holds the
# two in step.
REVISION = "df8ed95e44a70924450381e610770f9d656d1e15"


def fix_libs() -> None:
    import onnxruntime  # noqa: F401 — must be installed (parakeet extra)
    import sherpa_onnx

    ort_capi = Path(onnxruntime.__file__).parent / "capi"
    libs = sorted(ort_capi.glob("libonnxruntime.so.1.*"))
    if not libs:
        sys.exit(f"no libonnxruntime.so.1.* under {ort_capi}")
    target = Path(sherpa_onnx.__file__).parent.parent / "sherpa_onnx.libs" / "libonnxruntime.so"
    target.parent.mkdir(exist_ok=True)
    target.unlink(missing_ok=True)
    target.symlink_to(libs[-1])
    print(f"linked {target} -> {libs[-1]}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", default=REPO_ID, help="HF repo id (latency variant or fallback)")
    parser.add_argument(
        "--revision",
        default=None,
        help=f"upstream revision (default: the pin {REVISION[:12]} when --repo is the "
        "pinned repo; a variant repo resolves its own main unless given one)",
    )
    parser.add_argument(
        "--fix-libs", action="store_true", help="symlink libonnxruntime for sherpa-onnx"
    )
    args = parser.parse_args()

    if args.fix_libs:
        fix_libs()

    from huggingface_hub import snapshot_download

    # The pin belongs to REPO_ID. Passing --repo asks for a different export
    # (a latency variant, the Zipformer fallback), where this commit does not
    # exist - so fall back to that repo's main rather than failing.
    revision = args.revision or (REVISION if args.repo == REPO_ID else None)
    path = Path(snapshot_download(args.repo, revision=revision))
    missing = [
        f
        for f in (
            "encoder.int8.onnx",
            "decoder.int8.onnx",
            "joiner.int8.onnx",
            "tokens.txt",
        )
        if not (path / f).exists()
    ]
    if missing:
        sys.exit(f"download incomplete, missing: {', '.join(missing)}")
    total = sum(f.stat().st_size for f in path.glob("*.onnx"))
    print(f"sherpa model ready: {path} ({total / 1e6:.0f} MB onnx)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
