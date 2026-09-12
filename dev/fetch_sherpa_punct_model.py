"""Stage the English punctuation + truecasing model for the sherpa adapter.

    uv run python dev/fetch_sherpa_punct_model.py [--out-dir DIR]

Source: k2-fsa's `sherpa-onnx-online-punct-en-2024-08-06` (a conversion of
Edge-Punct-Casing, Apache-2.0, https://arxiv.org/abs/2407.13142) — a CNN-BiLSTM
that takes unpunctuated lowercase text and returns it punctuated and cased.

Why this and not the CT-Transformer: the streaming FastConformer this snap
serves is English and exports neither punctuation nor case (its `tokens.txt` is
1025 entries whose only punctuation is an apostrophe). CT-Transformer restores
punctuation but not case, and is Chinese-first at 72 MB int8; this one restores
both, in 7.5 MB, and was built for on-device streaming ASR.

Fetched from a GitHub release rather than the Hub — k2-fsa publishes the
punctuation models only there — so the pin is the archive's sha256 rather than
a revision: the `punctuation-models` tag it hangs off is mutable. Cached under
XDG_CACHE_HOME/myna/models and stamped, like dev/parakeet/fetch_parakeet_onnx.py.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import shutil
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ARCHIVE = "sherpa-onnx-online-punct-en-2024-08-06"
URL = (
    f"https://github.com/k2-fsa/sherpa-onnx/releases/download/punctuation-models/{ARCHIVE}.tar.bz2"
)
# The pin. A tag can move under a release asset, so identity is the hash of the
# bytes, checked on every stage - see dev/model-pin.sh for the same argument on
# the bash side, and dev/parakeet/qsilu/build.sh for the other sha256 pin.
SHA256 = "9f5e5a72c7d2829635bd074fce92b6bbd5b78da8a52e7ad8ed1be933f366b99d"
STAMP = f"{ARCHIVE} sha256:{SHA256}"
STAMP_FILE = "UPSTREAM_REVISION"
# The fp32 model.onnx (29 MB) is left in the archive: the adapter runs one
# 3.7 ms pass per committed segment, so there is nothing for the extra 22 MB
# to buy.
MODEL_FILES = ("model.int8.onnx", "bpe.vocab")


def default_model_dir() -> Path:
    cache = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache"))
    return cache / "myna" / "models" / ARCHIVE


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
            print(f"\r{dest.name}: {(have + out.tell()) / 1e6:.0f} MB", end="", file=sys.stderr)
    print(file=sys.stderr)


def staged(out_dir: Path) -> bool:
    """Whether ``out_dir`` holds *this* archive, not merely a staged one.

    Presence alone would let a cache from an older pin sit there forever: the
    files exist, so the download is skipped and the sha256 that would have
    caught the drift is never computed.
    """
    if not all((out_dir / f).exists() for f in MODEL_FILES):
        return False
    stamp = out_dir / STAMP_FILE
    return stamp.exists() and stamp.read_text(encoding="utf-8").strip() == STAMP


def stage(out_dir: Path) -> Path:
    """Download (resumable), verify, and extract into ``out_dir``. No-op when
    already staged — offline-safe after the first run."""
    if staged(out_dir):
        return out_dir
    out_dir.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out_dir.parent) as tmp:
        archive = Path(tmp) / "punct.tar.bz2"
        _download(URL, archive)
        if (digest := _sha256(archive)) != SHA256:
            raise RuntimeError(f"sha256 mismatch: {digest} != {SHA256} (pinned {URL})")
        with tarfile.open(archive) as tf:
            # Extract only what ships, by name: the archive is trusted (it just
            # hashed), but a member list is still input, and `filter="data"` is
            # only the default from 3.14.
            for name in MODEL_FILES:
                # Members are "./<ARCHIVE>/<name>", the prefix tar writes.
                tf.extract(f"./{ARCHIVE}/{name}", tmp, filter="data")
        out_dir.mkdir(exist_ok=True)
        for name in MODEL_FILES:
            shutil.move(str(Path(tmp) / ARCHIVE / name), out_dir / name)
    (out_dir / STAMP_FILE).write_text(f"{STAMP}\n", encoding="utf-8")
    return out_dir


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--out-dir",
        type=Path,
        default=None,
        help="stage here instead of the XDG cache (e.g. to pass as "
        "--sherpa-punct-model to myna-server)",
    )
    args = parser.parse_args()
    out_dir = stage(args.out_dir or default_model_dir())
    total = sum((out_dir / f).stat().st_size for f in MODEL_FILES)
    print(f"sherpa punctuation model ready: {out_dir} ({total / 1e6:.1f} MB)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
