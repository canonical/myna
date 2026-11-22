#!/usr/bin/python3
"""A myna-text-v1 share that passes for myna-text and answers as its mode says."""

import json
import os
import socketserver
import time

common = os.environ["SNAP_COMMON"]
share = os.path.join(common, "share", "provider")
os.makedirs(share, exist_ok=True)
sock = os.path.join(share, "myna.sock")
with open(os.path.join(share, "provider.env"), "w") as f:
    f.write(
        "SNAP_NAME=myna-text\nSNAP_INSTANCE_NAME=myna-text\nUNIX_SOCKET=myna.sock\nMYNA_ROLE=text\n"
    )
os.chmod(os.path.join(share, "provider.env"), 0o644)
MODEL = {
    "id": "fault-cleanup",
    "task": "text.cleanup",
    "covers": {"language": ["en"]},
    "accepts": [],
    "latency_ms": 5,
    "quality": 0.99,
    "class": "edit",
}


def mode() -> str:
    try:
        with open(os.path.join(common, "mode")) as f:
            return f.read().strip()
    except OSError:
        return "ok"


def answer(text: str) -> str | None:
    how = mode()
    if how == "chatty":
        return "Sure! Here is the cleaned text: " + text
    if how == "control":
        return text.replace(" ", "\n", 1) + "\x07\x1b[2J\r"
    if how == "overdelete":
        return " ".join(text.split()[:1])
    if how == "empty":
        return ""
    if how == "hang":
        time.sleep(3600)
    return text


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        for line in self.rfile:
            call = json.loads(line)
            if call.get("op") == "capabilities":
                reply = {"protocol": "myna-text-v1", "contract": 2, "models": [MODEL]}
            elif call.get("op") == "run":
                reply = {"text": answer(str(call.get("text", "")))}
            else:
                reply = {"ok": True}
            self.wfile.write(json.dumps(reply).encode() + b"\n")


if os.path.exists(sock):
    os.unlink(sock)
with socketserver.ThreadingUnixStreamServer(sock, Handler) as server:
    os.chmod(sock, 0o666)
    server.serve_forever()
