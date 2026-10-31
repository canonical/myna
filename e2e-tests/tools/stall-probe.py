#!/usr/bin/env python3
"""stall-probe.py gaps|vcpus FILE: until killed, log scheduling stalls too
short for cpu-sample.sh's one-second lines to show.

  gaps   wakes every 2 ms and logs each wakeup more than 15 ms late: how long
         this machine, or the vCPU under it, stopped running a thread. Run it
         under `chrt -f 99` and nothing inside the machine can delay it.
  vcpus  every 10 ms, adds up how long each QEMU vCPU thread waited for a
         host CPU while runnable (schedstat run_delay), and logs any thread
         kept waiting more than 5 ms of that: the host taking the vCPU.

Lines carry the wall clock as the journal's short-precise does, so a stall
can be laid against a capture loss.
"""

import glob
import re
import sys
import time


def stamp(t):
    return time.strftime("%H:%M:%S", time.localtime(t)) + f".{int(t % 1 * 1e6):06d}"


def gaps(out):
    period, late = 0.002, 0.015
    last = time.monotonic()
    while True:
        time.sleep(period)
        now = time.monotonic()
        if now - last - period > late:
            print(f"{stamp(time.time())} gap {(now - last) * 1000:.0f} ms", file=out, flush=True)
        last = now


def vcpu_threads():
    found = {}
    for comm in glob.glob("/proc/[0-9]*/task/[0-9]*/comm"):
        try:
            with open(comm) as f:
                name = f.read().strip()
        except OSError:
            continue
        if re.fullmatch(r"CPU \d+/KVM", name):
            found[comm[: -len("comm")] + "schedstat"] = name
    return found


def vcpus(out):
    threads, delays, scan = {}, {}, 0.0
    while True:
        if time.monotonic() - scan > 2:
            threads, scan = vcpu_threads(), time.monotonic()
        for path, name in threads.items():
            try:
                with open(path) as f:
                    delay = int(f.read().split()[1])
            except (OSError, IndexError, ValueError):
                continue
            before = delays.get(path)
            delays[path] = delay
            if before is not None and delay - before > 5_000_000:
                print(f"{stamp(time.time())} {name} waited {(delay - before) / 1e6:.0f} ms",
                      file=out, flush=True)
        time.sleep(0.01)


def main():
    mode, path = sys.argv[1], sys.argv[2]
    with open(path, "a") as out:
        print(f"{stamp(time.time())} start {mode}", file=out, flush=True)
        {"gaps": gaps, "vcpus": vcpus}[mode](out)


if __name__ == "__main__":
    main()
