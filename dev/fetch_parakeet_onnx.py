"""Stage the Parakeet TDT 0.6B v3 int8 ONNX weights (008 US3 / T023).

    uv run python dev/fetch_parakeet_onnx.py [--out-dir DIR]

Source: murmure's `parakeet-tdt-0.6b-v3-int8` bundle (GitHub release zip,
pinned + sha256-verified) — `nemo128.onnx` (mel preprocessor),
`encoder-model.int8.onnx`, `decoder_joint-model.int8.onnx` (TDT
decoder+joint), `vocab.txt`, in the layout the adapter expects.

Why murmure's bundle and not the istupakov HF export: the preprocessor,
decoder_joint and vocab are byte-identical, but istupakov's int8 *encoder*
collapses (blank output mid-audio) non-monotonically on some inputs
(12/14/18/20-22 s prefixes of stream-2277-01 fail; 15-17/19/28 s decode) —
the nemo128 preprocessor does utterance-global CMVN, so feature statistics
shift with window length and that quantization can't absorb the shift.
Murmure's re-quantized encoder decodes every probed length fully
(2026-07-29 discriminator runs). sherpa's k2 int8 export is intermediate
(22 s collapse) — murmure's is the only fully robust int8 encoder found.

Model: NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0), 25 languages, punctuation +
capitalisation. Cached under XDG_CACHE_HOME/myna/models and stamped with the
release it came from; re-running skips the download only when that stamp
matches the pin (offline-safe once staged, restaged when the pin moves).
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
import tempfile
import urllib.request
import zipfile
from pathlib import Path

# Pinned upstream release. Unlike the Hugging Face fetchers this one never
# floated - the URL names a release tag and the zip is sha256-verified - but
# staging was still presence-only, so a cache left from an older release
# survived a pin move unnoticed. STAMP records what a staged directory came
# from; see dev/model-pin.sh for the same mechanism on the bash side.
RELEASE = "1.2.0"
URL = (
    f"https://github.com/Kieirra/murmure-model/releases/download/{RELEASE}"
    "/parakeet-tdt-0.6b-v3-int8.zip"
)
SHA256 = "2adb3e2e6feaace71119eed506cb18401ac41b8daef1b6411a9e0ca5f12cacfe"
STAMP = f"murmure-model {RELEASE}"
STAMP_FILE = "UPSTREAM_REVISION"
MODEL_FILES = (
    "encoder-model.int8.onnx",
    "decoder_joint-model.int8.onnx",
    "nemo128.onnx",
    "vocab.txt",
)


def default_model_dir() -> Path:
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache / "myna" / "models" / "parakeet-tdt-0.6b-v3-int8"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def _download(url: str, dest: Path) -> None:
    """Resumable download (Range append) with progress on stderr."""
    have = dest.stat().st_size if dest.exists() else 0
    req = urllib.request.Request(url, headers={"Range": f"bytes={have}-"} if have else {})
    with urllib.request.urlopen(req, timeout=60) as resp, dest.open("ab") as out:
        while block := resp.read(1 << 20):
            out.write(block)
            print(
                f"\r{dest.name}: {(have + out.tell()) / 1e6:.0f} MB",
                end="",
                file=sys.stderr,
            )
    print(file=sys.stderr)


def staged(out_dir: Path) -> bool:
    """Whether ``out_dir`` holds this release - not merely *a* release.

    Presence alone would let a cache staged from an older pin sit there
    forever: the files exist, so the download is skipped and the sha256 that
    would have caught the drift is never computed.
    """
    if not all((out_dir / f).exists() for f in MODEL_FILES):
        return False
    stamp = out_dir / STAMP_FILE
    return stamp.exists() and stamp.read_text(encoding="utf-8").strip() == STAMP


def stage(out_dir: Path) -> Path:
    """Download (resumable), verify, and extract the weights into ``out_dir``.
    No-op when already staged — offline-safe after the first run."""
    if staged(out_dir):
        return out_dir
    out_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out_dir.parent) as tmp:
        zip_path = Path(tmp) / "model.zip"
        _download(URL, zip_path)
        if (digest := _sha256(zip_path)) != SHA256:
            raise RuntimeError(f"sha256 mismatch: {digest} != {SHA256} (pinned {URL})")
        with zipfile.ZipFile(zip_path) as zf:
            zf.extractall(tmp)
        extracted = Path(tmp) / "parakeet-tdt-0.6b-v3-int8"
        out_dir.mkdir(exist_ok=True)
        for name in MODEL_FILES:
            shutil.move(str(extracted / name), out_dir / name)
    (out_dir / STAMP_FILE).write_text(f"{STAMP}\n", encoding="utf-8")
    return out_dir


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="stage here instead of the XDG cache (e.g. a lab cache to pass "
        "as --model to myna-server --adapter parakeet)",
    )
    args = parser.parse_args()
    out_dir = stage(args.out_dir or default_model_dir())
    total = sum((out_dir / f).stat().st_size for f in MODEL_FILES)
    print(f"parakeet int8 ONNX ready: {out_dir} ({total / 1e6:.0f} MB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
