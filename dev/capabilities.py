"""Query a running UbuSTT socket for its capabilities (T24).

    uv run python dev/capabilities.py --socket /var/snap/whisper/common/run/ubustt.sock

Verification client: asks the server what it can do (models, input languages,
accepted audio formats, punctuation/translation) before opening a session, and
prints it. Mirrors dev/transcribe.py but for the discovery surface.
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from myna.core import WsUnixClient  # noqa: E402


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, type=Path)
    args = parser.parse_args()

    caps = await WsUnixClient(args.socket).capabilities()
    print(f"models       : {', '.join(caps.models) or '(unspecified)'}")
    print(f"languages    : {', '.join(caps.languages)}")
    fmts = ", ".join(
        f"{f.sample_rate_hz} Hz/{f.channels}ch/{8 * f.sample_width_bytes}-bit"
        for f in caps.input_formats
    )
    print(f"input formats: {fmts}")
    print(f"punctuation  : {caps.punctuation}")
    print(f"translation  : {caps.translation}")


if __name__ == "__main__":
    asyncio.run(main())
