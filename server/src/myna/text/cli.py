"""``myna-text``: the daemon of the ``myna-text`` snap."""

from __future__ import annotations

import argparse
import logging
import os
import shutil
import signal
import sys
import threading
from collections.abc import Mapping, Sequence
from pathlib import Path

from myna.server import provider_env
from myna.text import components
from myna.text.protocol import Provider
from myna.text.server import Server

ROLE = "text"

log = logging.getLogger("myna.text")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="myna-text", description=__doc__)
    parser.add_argument("--socket", type=Path, required=True, help="the Unix socket to serve on")
    parser.add_argument(
        "--components",
        type=Path,
        help="directory of model components (default: $SNAP_COMPONENTS)",
    )
    parser.add_argument(
        "--bergamot",
        type=Path,
        help="the myna-bergamot binary (default: beside this interpreter, else on PATH)",
    )
    parser.add_argument(
        "--share-provider",
        action="store_true",
        help="write provider.env beside the socket (SNAP_NAME, SNAP_INSTANCE_NAME, "
        "UNIX_SOCKET, MYNA_ROLE=text)",
    )
    parser.add_argument("--socket-mode", default="0666", help="octal permission bits of the socket")
    return parser


def _bergamot(given: Path | None) -> Path:
    if given is not None:
        return given
    beside = Path(sys.executable).parent / "myna-bergamot"
    return beside if beside.is_file() else Path(shutil.which("myna-bergamot") or beside)


def _components(given: Path | None, environ: Mapping[str, str]) -> Path:
    if given is not None:
        return given
    if environ.get("SNAP_COMPONENTS"):
        return Path(environ["SNAP_COMPONENTS"])
    raise SystemExit("myna-text: pass --components or run it in the snap (SNAP_COMPONENTS)")


def serve(args: argparse.Namespace, environ: Mapping[str, str], stop: threading.Event) -> None:
    """Serve until ``stop`` is set."""
    root = _components(args.components, environ)
    provider = Provider(components.finder(root, _bergamot(args.bergamot)))
    server = Server(args.socket, provider, int(args.socket_mode, 8))
    try:
        if args.share_provider:
            provider_env.share(args.socket, environ, role=ROLE)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        log.info("serving %s from %s (pid %d)", args.socket, root, os.getpid())
        stop.wait()
        server.shutdown()
        thread.join()
    finally:
        server.server_close()
        provider.close()


def main(argv: Sequence[str] | None = None, environ: Mapping[str, str] = os.environ) -> int:
    args = build_parser().parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    stop = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    serve(args, environ, stop)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
