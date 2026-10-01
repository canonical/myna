#!/usr/bin/python3
"""A snapd socket stand-in that forwards every request to /run/snapd.socket
and makes an install served from snapd's blob cache look like a download.

Usage: slow-snapd.py SOCKET RAMP_S SNAP

For RAMP_S seconds after GET /v2/changes/<id> first returns a change that
names SNAP, the change reads as unfinished, with one download task whose
bytes climb linearly from 0 to 100%. After that the real answer passes
unchanged. Only the app's mount namespace sees SOCKET (shot-remote.sh binds
it over /run/snapd.socket), so `snap` and everything else on the host keep
talking to snapd directly.
"""
import http.client
import json
import os
import re
import socket
import socketserver
import sys
import time

REAL = "/run/snapd.socket"
SOCK, RAMP, SNAP = sys.argv[1], float(sys.argv[2]), sys.argv[3]
# Above every expected size, so the app's percentage is the ramp's.
TOTAL = 5_000_000_000
first_seen = {}


def log(line):
    print(f"{time.strftime('%H:%M:%S')} {line}", flush=True)


class Snapd(http.client.HTTPConnection):
    def __init__(self):
        super().__init__("localhost", timeout=900)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(900)
        self.sock.connect(REAL)


def rewrite(path, body):
    match = re.fullmatch(r"/v2/changes/(\d+)", path.split("?")[0])
    if not match:
        return body
    try:
        envelope = json.loads(body)
    except ValueError:
        return body
    change = envelope.get("result")
    if not isinstance(change, dict) or SNAP not in json.dumps(change):
        return body
    cid = match.group(1)
    start = first_seen.setdefault(cid, time.monotonic())
    frac = (time.monotonic() - start) / RAMP
    if frac >= 1:
        if first_seen.get(cid + "-done") is None:
            first_seen[cid + "-done"] = 1
            log(f"change {cid}: ramp over, real status {change.get('status')}")
        return body
    change["status"] = "Doing"
    change["ready"] = False
    change.pop("ready-time", None)
    change.pop("err", None)
    tasks = change.setdefault("tasks", [])
    downloads = [t for t in tasks if str(t.get("kind", "")).startswith("download-")]
    if not downloads:
        downloads = [{"id": "0", "kind": "download-snap", "summary": f'Download snap "{SNAP}"'}]
        tasks.insert(0, downloads[0])
    for task in downloads[1:]:
        task["progress"] = {"label": "", "done": 0, "total": 1}
    lead = downloads[0]
    lead["status"] = "Doing"
    lead["progress"] = {"label": SNAP, "done": int(TOTAL * frac), "total": TOTAL}
    log(f"change {cid}: {int(frac * 100)}%")
    return json.dumps(envelope).encode()


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        while True:
            line = self.rfile.readline()
            if not line:
                return
            method, path, _ = line.decode().split(" ", 2)
            headers = {}
            while True:
                header = self.rfile.readline().decode()
                if header in ("\r\n", "\n", ""):
                    break
                name, value = header.split(":", 1)
                headers[name.strip()] = value.strip()
            length = int(headers.get("Content-Length", 0) or 0)
            body = self.rfile.read(length) if length else None
            close = headers.get("Connection", "").lower() == "close"
            headers.pop("Connection", None)
            snapd = Snapd()
            try:
                snapd.request(method, path, body=body, headers=headers)
                response = snapd.getresponse()
                data = response.read()
            finally:
                snapd.close()
            log(f"{method} {path} -> {response.status} ({len(data)} B)")
            if method == "GET":
                data = rewrite(path, data)
            out = [f"HTTP/1.1 {response.status} {response.reason}"]
            for name, value in response.getheaders():
                if name.lower() not in ("content-length", "transfer-encoding", "connection"):
                    out.append(f"{name}: {value}")
            out.append(f"Content-Length: {len(data)}")
            out.append("Connection: close" if close else "Connection: keep-alive")
            self.wfile.write(("\r\n".join(out) + "\r\n\r\n").encode() + data)
            self.wfile.flush()
            if close:
                return


class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


if os.path.exists(SOCK):
    os.unlink(SOCK)
server = Server(SOCK, Handler)
os.chmod(SOCK, 0o666)
log(f"forwarding {SOCK} to {REAL}, {SNAP} downloads over {RAMP} s")
server.serve_forever()
