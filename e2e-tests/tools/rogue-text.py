#!/usr/bin/python3
"""A myna-text-v1 share of a snap name Myna does not list, shouting its output."""

import json
import os
import socketserver

share = os.path.join(os.environ["SNAP_COMMON"], "share", "provider")
os.makedirs(share, exist_ok=True)
sock = os.path.join(share, "myna.sock")
with open(os.path.join(share, "provider.env"), "w") as f:
    f.write(
        f"SNAP_NAME={os.environ['SNAP_NAME']}\nSNAP_INSTANCE_NAME={os.environ['SNAP_INSTANCE_NAME']}\n"
        "UNIX_SOCKET=myna.sock\nMYNA_ROLE=text\n"
    )
os.chmod(os.path.join(share, "provider.env"), 0o644)
MODEL = {
    "id": "rogue-cleanup",
    "task": "text.cleanup",
    "covers": {"language": ["en"]},
    "accepts": [],
    "latency_ms": 1,
    "quality": 0.99,
    "class": "edit",
}


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            call = json.loads(line)
            if call.get("op") == "capabilities":
                reply = {"protocol": "myna-text-v1", "contract": 2, "models": [MODEL]}
            elif call.get("op") == "run":
                reply = {"text": str(call.get("text", "")).upper()}
            else:
                reply = {"ok": True}
            self.wfile.write(json.dumps(reply).encode() + b"\n")


if os.path.exists(sock):
    os.unlink(sock)
with socketserver.ThreadingUnixStreamServer(sock, Handler) as server:
    os.chmod(sock, 0o666)
    server.serve_forever()
