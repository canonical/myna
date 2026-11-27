"""The daemon every text-provider snap runs: a component scan behind a socket."""

from __future__ import annotations

import argparse
import logging
import os
import signal
import threading
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

from myna.server import provider_env
from myna.text import components
from myna.text.components import Builder
from myna.text.protocol import Provider
from myna.text.server import Server

ROLE = "text"

log = logging.getLogger("myna.text")

#: The component kinds to serve, given the parsed arguments.
Kinds = Callable[[argparse.Namespace], Mapping[str, Builder]]


def build_parser(prog: str, description: str | None) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog=prog, description=description)
    parser.add_argument("--socket", type=Path, required=True, help="the Unix socket to serve on")
    parser.add_argument(
        "--components",
        type=Path,
        help="directory of model components (default: $SNAP_COMPONENTS)",
    )
    parser.add_argument(
        "--share-provider",
        action="store_true",
        help="write provider.env beside the socket (SNAP_NAME, SNAP_INSTANCE_NAME, "
        "UNIX_SOCKET, MYNA_ROLE=text)",
    )
    parser.add_argument("--socket-mode", default="0666", help="octal permission bits of the socket")
    return parser


def components_dir(given: Path | None, environ: Mapping[str, str]) -> Path:
    if given is not None:
        return given
    if environ.get("SNAP_COMPONENTS"):
        return Path(environ["SNAP_COMPONENTS"])
    raise SystemExit("pass --components or run it in the snap (SNAP_COMPONENTS)")


def serve(
    args: argparse.Namespace,
    environ: Mapping[str, str],
    stop: threading.Event,
    kinds: Mapping[str, Builder],
) -> None:
    """Serve until ``stop`` is set."""
    root = components_dir(args.components, environ)
    provider = Provider(components.finder(root, kinds))
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


def run(parser: argparse.ArgumentParser, argv: Sequence[str] | None, kinds: Kinds) -> int:
    """Parse ``argv`` and serve until a termination signal."""
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    stop = threading.Event()
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, lambda *_: stop.set())
    serve(args, os.environ, stop, kinds(args))
    return 0
