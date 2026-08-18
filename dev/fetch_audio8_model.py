"""Stage Audio8-ASR-0.1B ONNX runtime + model bundle (feature 010).

    uv run python dev/fetch_audio8_model.py --accept-license "CC-BY-NC-4.0" \
        [--profile dev|snap|full] [--target DIR]

Downloads the publisher's self-contained ONNX runtime release from Hugging Face
(``Audio8/Audio8-ASR-0.1B-onnx-runtime``) — the engine source
(``asr_onnx_runtime.py`` + ``hotword/``) AND the ``model_bundle/`` graphs. The
adapter loads the engine from the staged directory (``AUDIO8_MODEL_DIR`` env
override, else the HF cache snapshot); nothing CC-BY-NC-licensed is committed
to the git tree (myna is GPLv3; research.md Decision 2).

License: the checkpoint AND the runtime source are CC-BY-NC-4.0
(non-commercial). This script surfaces that license and requires explicit
acknowledgment before downloading; compliance with the license is the
responsibility of whoever integrates or distributes the staged artifacts
(FR-014).

Profiles (measured from the HF repo; fp32 graphs are reference-only and
excluded from dev/snap):
  dev  — int8 decoder + int8 audio tower + shared weights + engine (~773 MB)
  snap — dev + int4 decoder graphs (~886 MB)
  full — everything, incl. fp32 graphs and lm_logits (~3 GB)
"""

from __future__ import annotations

import argparse
import shutil
import subprocess
import sys
from pathlib import Path

REPO_ID = "Audio8/Audio8-ASR-0.1B-onnx-runtime"
LICENSE = "CC-BY-NC-4.0"

# Engine source + license + bundle metadata/tokenizer, required by every profile.
_BASE_PATTERNS = (
    "asr_onnx_runtime.py",
    "hotword/*.py",
    "LICENSE",
    "model_bundle/metadata.json",
    "model_bundle/config.json",
    "model_bundle/preprocessor_config.json",
    "model_bundle/qwen3_asr_feature_extractor/*.json",
    "model_bundle/tokenizer.json",
    "model_bundle/vocab.json",
    "model_bundle/merges.txt",
    "model_bundle/added_tokens.json",
    "model_bundle/weights/*",
    # int8 graphs (the engine's default; OnnxCacheAsrEngine load_lm_session=False
    # never touches lm_logits.onnx — research.md Decision 1).
    "model_bundle/audio_hidden_int8.onnx",
    "model_bundle/lm_cache_prefill_int8.onnx*",
    "model_bundle/lm_cache_decode_int8.onnx*",
)

_PROFILES: dict[str, tuple[str, ...]] = {
    "dev": _BASE_PATTERNS,
    "snap": _BASE_PATTERNS + (
        "model_bundle/lm_cache_prefill_int4.onnx*",
        "model_bundle/lm_cache_decode_int4.onnx*",
    ),
    "full": _BASE_PATTERNS + (
        "model_bundle/audio_hidden.onnx",
        "model_bundle/lm_cache_prefill.onnx",
        "model_bundle/lm_cache_decode.onnx",
        "model_bundle/lm_logits.onnx",
        "model_bundle/lm_cache_prefill_int4.onnx*",
        "model_bundle/lm_cache_decode_int4.onnx*",
    ),
}

_LICENSE_NOTICE = f"""
The Audio8-ASR-0.1B model bundle and its ONNX runtime source are released
under the Creative Commons Attribution-NonCommercial 4.0 license
(CC-BY-NC-4.0). Commercial use is NOT permitted under this license.

By passing --accept-license you acknowledge that YOU (the integrator /
distributor) are responsible for license compliance for any downstream use
or redistribution of the staged artifacts. Myna tooling surfaces this
notice; it does not and cannot grant rights.

Re-run with:  --accept-license "{LICENSE}"
"""


def _ensure_hf_hub() -> None:
    try:
        import huggingface_hub  # noqa: F401
    except ImportError:
        print("huggingface_hub not installed. Installing...", file=sys.stderr)
        package = "huggingface_hub>=0.24"
        try:
            subprocess.check_call(["uv", "pip", "install", "--quiet", package], stdout=sys.stderr)
        except (FileNotFoundError, subprocess.CalledProcessError):
            subprocess.check_call(
                [sys.executable, "-m", "pip", "install", "--break-system-packages", package],
                stdout=sys.stderr,
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--accept-license",
        default=None,
        help=f'acknowledge the non-commercial license, e.g. --accept-license "{LICENSE}"',
    )
    parser.add_argument("--profile", choices=tuple(_PROFILES), default="dev")
    parser.add_argument(
        "--target",
        type=Path,
        default=None,
        help="flat output directory (default: leave staged in the HF cache; "
        "the adapter locates it via the cache snapshot or $AUDIO8_MODEL_DIR)",
    )
    args = parser.parse_args()

    if args.accept_license != LICENSE:
        print(_LICENSE_NOTICE, file=sys.stderr)
        return 2

    _ensure_hf_hub()
    from huggingface_hub import snapshot_download

    allow = list(_PROFILES[args.profile])
    print(f"⬇️  Downloading {REPO_ID} (profile={args.profile}, {len(allow)} patterns)...")
    snapshot_dir = Path(
        snapshot_download(REPO_ID, allow_patterns=allow, repo_type="model")
    )
    print(f"   Staged at: {snapshot_dir}")

    required = ("asr_onnx_runtime.py", "model_bundle/metadata.json", "model_bundle/tokenizer.json")
    missing = [f for f in required if not (snapshot_dir / f).exists()]
    if missing:
        print(f"❌ Download incomplete, missing: {', '.join(missing)}", file=sys.stderr)
        return 1

    if args.target is not None:
        args.target.mkdir(parents=True, exist_ok=True)
        for src in snapshot_dir.rglob("*"):
            if src.is_file():
                rel = src.relative_to(snapshot_dir)
                dst = args.target / rel
                dst.parent.mkdir(parents=True, exist_ok=True)
                if not dst.exists() or dst.stat().st_size != src.stat().st_size:
                    shutil.copy2(src, dst)
        print(f"✅ Audio8 runtime + bundle copied to {args.target}")
    else:
        print(f"✅ Audio8 runtime + bundle ready at {snapshot_dir}")
        print(f"   (export AUDIO8_MODEL_DIR={snapshot_dir} to point the adapter at it)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
