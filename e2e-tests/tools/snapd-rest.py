#!/usr/bin/env python3
"""Talk to snapd's REST API as the calling user, the way Myna Settings would.

  snapd-rest.py flag                      # experimental.user-daemons, unprivileged
  snapd-rest.py set-flag true|false       # PUT /v2/snaps/system/conf
  snapd-rest.py install NAME [CHANNEL]    # POST /v2/snaps/NAME, default latest/edge
  snapd-rest.py remove NAME
  snapd-rest.py changes                   # in-progress changes with task progress

install/remove/set-flag follow the change to the end and print one line per
second: every in-progress change and its tasks' progress (done/total), so a
snapctl-install change spawned by a hook shows too. Every request sends
X-Allow-Interaction: true, so snapd asks polkit.
"""
import http.client
import json
import socket
import sys
import time

SOCKET = "/run/snapd.socket"


class Conn(http.client.HTTPConnection):
    def __init__(self):
        super().__init__("localhost", timeout=60)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(SOCKET)


def call(method, path, body=None):
    conn = Conn()
    headers = {"X-Allow-Interaction": "true"}
    data = None
    if body is not None:
        data = json.dumps(body)
        headers["Content-Type"] = "application/json"
    conn.request(method, path, body=data, headers=headers)
    response = json.loads(conn.getresponse().read())
    conn.close()
    return response


def flag():
    info = call("GET", "/v2/system-info")["result"]["features"].get("user-daemons", {})
    return info.get("enabled", False)


def in_progress():
    # select=in-progress also lists ready changes whose status is Hold (an
    # auto-refresh held for a snap since removed), which never finish.
    return [c for c in call("GET", "/v2/changes?select=in-progress")["result"] if not c.get("ready")]


def describe(change):
    parts = []
    for task in change.get("tasks", []):
        if task["status"] not in ("Doing", "Wait"):
            continue
        progress = task.get("progress", {})
        parts.append(
            f'{task["kind"]}[{task["status"]}] {progress.get("label", "")} '
            f'{progress.get("done", 0)}/{progress.get("total", 0)}'
        )
    return f'#{change["id"]} {change["kind"]} "{change["summary"]}": ' + "; ".join(parts)


def follow(response):
    print(json.dumps({k: response.get(k) for k in ("type", "status-code", "change", "result")}))
    change_id = response.get("change")
    if not change_id:
        return 0 if response.get("type") != "error" else 1
    start = time.monotonic()
    while True:
        for change in in_progress():
            print(f"{time.monotonic() - start:7.1f}s {describe(change)}", flush=True)
        change = call("GET", f"/v2/changes/{change_id}")["result"]
        if change["ready"]:
            break
        time.sleep(1)
    # A hook may have started its own change (snapctl-install) that outlives ours.
    while True:
        pending = in_progress()
        if not pending:
            break
        for other in pending:
            print(f"{time.monotonic() - start:7.1f}s {describe(other)}", flush=True)
        time.sleep(1)
    print(f'{time.monotonic() - start:7.1f}s change {change_id} {change["status"]} {change.get("err", "")}')
    return 0 if change["status"] == "Done" else 1


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    command, args = argv[0], argv[1:]
    if command == "flag":
        print("true" if flag() else "false")
        return 0
    if command == "set-flag":
        value = args[0] == "true"
        return follow(call("PUT", "/v2/snaps/system/conf", {"experimental.user-daemons": value}))
    if command == "install":
        channel = args[1] if len(args) > 1 else "latest/edge"
        return follow(call("POST", f"/v2/snaps/{args[0]}", {"action": "install", "channel": channel}))
    if command == "remove":
        return follow(call("POST", f"/v2/snaps/{args[0]}", {"action": "remove"}))
    if command == "changes":
        for change in in_progress():
            print(describe(change))
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
