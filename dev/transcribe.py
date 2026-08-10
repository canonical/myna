"""Transcribe a fixture clip (or any WAV) against a running UbuSTT socket.

    uv run python dev/transcribe.py --socket /var/snap/whisper/common/run/ubustt.sock quiet-weather
    uv run python dev/transcribe.py --socket /tmp/ubustt.sock --wav some.wav

Verification client for the whisper snap / myna-server: streams the audio at
real-time pace (like live dictation), prints the events as they arrive, then
the latency metrics.
"""

from __future__ import annotations

import argparse
import asyncio
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from myna.core import SessionConfig, WsUnixClient  # noqa: E402
from myna.testbed import Harness, WavFileSource, load_manifest  # noqa: E402
from myna.testbed.adapter import Candidate  # noqa: E402


def pick_source(args: argparse.Namespace) -> tuple[WavFileSource, str | None]:
    if args.wav:
        return WavFileSource(args.wav, realtime=not args.batch), None
    clips = {c.id: c for c in load_manifest(REPO_ROOT / "server" / "fixtures" / "manifest.json")}
    if args.clip not in clips:
        raise SystemExit(f"unknown clip {args.clip!r}; available: {', '.join(sorted(clips))}")
    clip = clips[args.clip]
    return clip.open_source(realtime=not args.batch), clip.text


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("clip", nargs="?", default="quiet-weather", help="fixture clip id")
    parser.add_argument("--socket", required=True, type=Path)
    parser.add_argument("--wav", type=Path, help="transcribe a WAV file instead of a fixture clip")
    parser.add_argument("--language", default="en")
    parser.add_argument("--batch", action="store_true", help="stream as fast as possible")
    args = parser.parse_args()

    source, reference = pick_source(args)
    pace = "as fast as possible" if args.batch else "at real-time pace"
    print(f"streaming audio {pace} to {args.socket} — events:")
    print("(a cold server may download/load the model first; watch `snap logs -f whisper.server`)")

    def show(te) -> None:
        ev = te.event
        if ev.type == "transcription.error":
            # An error carries code + message, not text/snippet — show them, or
            # the failure reads as an empty string and the reason is lost.
            detail = (
                f"{getattr(ev, 'code', 'internal')}: {getattr(ev, 'message', '') or '(no message)'}"
            )
        elif ev.type == "transcription.progress":
            phase = getattr(ev, "phase", "")
            snippet = getattr(ev, "snippet", None) or ""
            detail = f"[{phase}] {snippet}".rstrip()
        else:
            detail = getattr(ev, "text", None) or ""
        print(f"  {te.t:7.3f}s  {ev.type:24} {detail!r}", flush=True)

    record = await Harness().run(
        client=WsUnixClient(args.socket),
        candidate=Candidate(model="?", engine="remote", streaming_strategy="?"),
        source=source,
        config=SessionConfig(audio_format=source.format, language=args.language),
        on_event=show,
    )

    m = record.metrics
    print(f"\naudio duration : {record.audio_duration_seconds:.2f}s")
    print(f"first event    : {m.time_to_first_event:.3f}s" if m.time_to_first_event else "")
    print(
        f"finalize       : {m.finalize_latency:.3f}s after end of audio"
        if m.finalize_latency
        else ""
    )
    print(f"transcript     : {record.transcript!r}")
    if reference:
        print(f"reference      : {reference!r}")


if __name__ == "__main__":
    asyncio.run(main())
