"""Command-line entry point for the UbuSTT server.

    myna-server --socket /path/to/ubustt.sock --model tiny
    python -m myna.server --socket ... --model base --preload

Serves transcription sessions over WebSocket on the given Unix socket until
SIGTERM/SIGINT. Model weights load lazily on the first session by default;
``--preload`` loads (and warms) at startup instead, trading idle memory for
first-session latency — the trade-off IE114 leaves to configuration.

Privacy: transcription content is never logged.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import logging
import os
import signal
import sys
from pathlib import Path

log = logging.getLogger("myna.server")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="myna-server", description=__doc__)
    parser.add_argument("--socket", required=True, type=Path, help="Unix socket path to serve on")
    parser.add_argument("--model", default="tiny", help="Whisper model size or CTranslate2 model path")
    parser.add_argument("--device", default="cpu", choices=("cpu", "cuda"), help="inference device")
    parser.add_argument("--compute-type", default="default", help="CTranslate2 compute type")
    parser.add_argument(
        "--socket-mode",
        default="0666",
        help="octal permission bits for the socket file (default world-connectable; "
        "tighten via dir permissions or dedicated group in production)",
    )
    parser.add_argument("--preload", action="store_true", help="load the model at startup")
    return parser


async def serve(args: argparse.Namespace) -> None:
    # NOTE(charles): Imported here so --help works without the whisper extra installed.
    try:
        from myna.testbed.whisper import FasterWhisperAdapter
    except ModuleNotFoundError as exc:
        raise SystemExit(
            f"missing dependency ({exc.name}) — install myna with the 'whisper' extra"
        ) from exc

    from myna.core import serve_unix

    adapter = FasterWhisperAdapter(
        args.model, device=args.device, compute_type=args.compute_type
    )
    if args.preload:
        log.info("preloading model %s on %s", args.model, args.device)
        await adapter._load_model()

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, stop.set)

    args.socket.parent.mkdir(parents=True, exist_ok=True)
    # remove a stale socket left by an unclean shutdown, or bind() fails
    with contextlib.suppress(FileNotFoundError):
        args.socket.unlink()
    async with serve_unix(adapter, args.socket):
        os.chmod(args.socket, int(args.socket_mode, 8))
        log.info(
            "serving model=%s device=%s on %s (pid %d)",
            args.model,
            args.device,
            args.socket,
            os.getpid(),
        )
        await stop.wait()
    log.info("shutting down")
    with contextlib.suppress(FileNotFoundError):
        args.socket.unlink()


def main(argv: list[str] | None = None) -> int:
    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(name)s %(levelname)s %(message)s"
    )
    args = build_parser().parse_args(argv)
    try:
        asyncio.run(serve(args))
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main())
