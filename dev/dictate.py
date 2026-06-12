"""Speak into the microphone and see the transcription (live demo).

    uv run python dev/dictate.py --socket /var/snap/whisper/common/run/ubustt.sock

Push-to-talk against a running UbuSTT snap: press Enter to start listening,
speak, press Enter again to transcribe. Ctrl-C to quit. This is the audio-push
model end to end — the client (here) captures from PipeWire and pushes PCM to
the service, which has no microphone access of its own.

With the commit-on-finalize adapter the committed text appears when you stop
talking (progress dots show liveness meanwhile); true mid-speech streaming is
T08. Audio is never written to disk.
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from myna.core import SessionConfig, WsUnixClient  # noqa: E402
from myna.testbed import Harness, MicSource  # noqa: E402
from myna.testbed.adapter import Candidate  # noqa: E402


def show(te) -> None:
    e = te.event
    if e.type == "transcription.progress":
        print("·", end="", flush=True)  # liveness while listening/decoding
    elif e.type == "transcription.final":
        print(f"\n  » {e.text}", flush=True)
    elif e.type == "transcription.error":
        print(f"\n  [error] {e.code}: {e.message}", flush=True)


async def one_utterance(args, loop) -> None:
    mic = MicSource(sample_rate_hz=args.rate, target=args.target)
    print("🎤 listening — speak, then press Enter (Ctrl-C to quit)")
    run = asyncio.ensure_future(
        Harness().run(
            client=WsUnixClient(args.socket),
            candidate=Candidate(model="mic", engine="snap", streaming_strategy="?"),
            source=mic,
            config=SessionConfig(audio_format=mic.format, language=args.language),
            on_event=show,
        )
    )

    stop = asyncio.Event()
    loop.add_reader(sys.stdin.fileno(), lambda: (sys.stdin.readline(), stop.set()))
    try:
        await stop.wait()
    finally:
        loop.remove_reader(sys.stdin.fileno())

    mic.stop()
    record = await run
    m = record.metrics
    took = f" ({m.finalize_latency:.2f}s)" if m.finalize_latency else ""
    print(f"  transcript: {record.transcript!r}{took}\n")


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True, type=Path)
    parser.add_argument("--language", default="en")
    parser.add_argument("--rate", type=int, default=16_000, help="capture sample rate (Hz)")
    parser.add_argument("--target", help="PipeWire source node (name or serial); default auto")
    parser.add_argument("--once", action="store_true", help="transcribe one utterance and exit")
    args = parser.parse_args()

    loop = asyncio.get_running_loop()
    print(f"dictating to {args.socket}")
    try:
        while True:
            await one_utterance(args, loop)
            if args.once:
                break
    except (KeyboardInterrupt, asyncio.CancelledError):
        print("\nbye")


if __name__ == "__main__":
    try:
        asyncio.run(main())
    except KeyboardInterrupt:
        print("\nbye")
