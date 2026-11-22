#!/usr/bin/env python3
"""Drive a running myna-translation through its socket: capabilities, warm, run.

    translation-snap/dev/smoke.py [SOCKET]

SOCKET defaults to the snap's share, /var/snap/myna-translation/common/share/
provider/text.sock. It prints what the daemon offers and, per model, the time
of the warm call and the p50/p95 of repeated runs, exiting 1 if no model is
offered, one refuses a call, or exceeds the latency it declares.
"""

import json
import socket
import sys
import time

SOCKET = "/var/snap/myna-translation/common/share/provider/text.sock"

SENTENCES = [
    "Hello, how are you today?",
    "I would like to book a table for two at eight o'clock.",
    "The meeting is on Monday. Please bring the report.",
    "She said that the weather would be better tomorrow.",
    "Can you send me the address of the restaurant?",
]


def call(path: str, request: dict, timeout: float = 30.0) -> tuple[dict, float]:
    started = time.perf_counter()
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout)
        s.connect(path)
        s.sendall(json.dumps(request).encode() + b"\n")
        reply = s.makefile("rb").readline()
    return json.loads(reply), (time.perf_counter() - started) * 1000


def main() -> int:
    path = sys.argv[1] if len(sys.argv) > 1 else SOCKET
    caps, ms = call(path, {"op": "capabilities"})
    print(f"capabilities in {ms:.1f} ms: protocol={caps.get('protocol')}")
    failed = caps.get("protocol") != "myna-text-v1" or not caps.get("models")
    for m in caps.get("models", []):
        print("  " + json.dumps(m))
        failed |= m["task"] != "text.translate"
        params = {k: v[0] for k, v in m["covers"].items()}
        warm, warm_ms = call(path, {"op": "warm", "model": m["id"], "task": m["task"],
                                    "params": params})
        print(f"{m['id']}: warm {warm} in {warm_ms:.0f} ms")
        times = []
        for text in SENTENCES * 8:
            reply, run_ms = call(
                path, {"op": "run", "model": m["id"], "task": m["task"], "params": params,
                       "text": text, "deadline_ms": 1000})
            times.append(run_ms)
            if "text" not in reply:
                print(f"  refused: {reply}")
                failed = True
            elif len(times) <= len(SENTENCES):
                print(f"  {text!r} -> {reply['text']!r} ({run_ms:.1f} ms)")
        times.sort()
        p50, p95 = times[len(times) // 2], times[int(len(times) * 0.95)]
        print(f"  {len(times)} runs: p50 {p50:.1f} ms, p95 {p95:.1f} ms "
              f"(declared {m['latency_ms']} ms)")
        failed |= not warm.get("ok") or p95 > m["latency_ms"]
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
