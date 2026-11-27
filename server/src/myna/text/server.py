"""Serve a :class:`~myna.text.protocol.Provider` on a Unix socket."""

from __future__ import annotations

import contextlib
import json
import os
import socketserver
from pathlib import Path

from myna.text.protocol import Provider

#: A request line longer than this is refused and the connection dropped: a
#: sentence, or a whole dictation, is a few kilobytes.
MAX_LINE = 1 << 20


class _Handler(socketserver.StreamRequestHandler):
    server: Server

    def handle(self) -> None:
        while True:
            line = self.rfile.readline(MAX_LINE + 1)
            if not line:
                return
            if len(line) > MAX_LINE and not line.endswith(b"\n"):
                self._send({"error": "request too long"})
                return
            try:
                call = json.loads(line)
            except ValueError:
                self._send({"error": "unreadable request"})
                continue
            self._send(self.server.provider.handle(call))

    def _send(self, reply: dict[str, object]) -> None:
        with contextlib.suppress(OSError):
            self.wfile.write(json.dumps(reply, ensure_ascii=False).encode("utf-8") + b"\n")
            self.wfile.flush()


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True

    def __init__(self, path: Path, provider: Provider, mode: int = 0o666) -> None:
        self.provider = provider
        path.parent.mkdir(parents=True, exist_ok=True)
        with contextlib.suppress(FileNotFoundError):  # a stale socket makes bind fail
            path.unlink()
        super().__init__(os.fspath(path), _Handler)
        os.chmod(path, mode)

    def server_bind(self) -> None:
        # Bind owner-only, then widen with the requested mode once it exists.
        old = os.umask(0o077)
        try:
            super().server_bind()
        finally:
            os.umask(old)
