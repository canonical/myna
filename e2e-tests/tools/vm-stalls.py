#!/usr/bin/env python3
"""vm-stalls.py JOURNAL STALLS: whether every capture loss in a suite's
journal came right after the VM itself stopped (stall-probe.py's gaps log).

A FIFO 99 thread in the guest that wakes late was not run by the vCPU under
it, which nothing in the guest can cause. A capture loss reported within
WINDOW of such a stall is the machine's, not Myna's: the daemon confirms a
deficit LOSS_CONFIRM (200 ms) after it opens.

Exit 0 when there was a loss and every one is explained, 1 when one is not,
2 when there was none (or no stall log to judge by). One line per loss.
"""

import re
import sys

WINDOW = 1.0
LOSS = re.compile(r" (\d\d:\d\d:\d\d\.\d+) \S+ \S+: \[myna [^]]*\] ctrl: capture lost")
AMOUNT = re.compile(r"(\d+) ms of audio was lost")
GAP = re.compile(r"^(\d\d:\d\d:\d\d\.\d+) gap (\d+) ms")


def secs(t):
    h, m, s = t.split(":")
    return int(h) * 3600 + int(m) * 60 + float(s)


def main():
    journal, stalls = sys.argv[1], sys.argv[2]
    try:
        with open(stalls) as f:
            gaps = [(secs(m[1]), int(m[2])) for m in map(GAP.match, f) if m]
    except OSError:
        print("vm-stalls: no stall log")
        return 2
    with open(journal, errors="replace") as f:
        losses = [(m[1], AMOUNT.search(line)) for line in f if (m := LOSS.search(line))]
    if not losses:
        return 2
    unexplained = 0
    for when, amount in losses:
        at = secs(when)
        near = [ms for t, ms in gaps if at - WINDOW <= t <= at]
        lost = f"{amount[1]} ms" if amount else "capture"
        if near:
            print(f"vm-stalls: {lost} lost at {when}, after VM stalls of {near} ms")
        else:
            print(f"vm-stalls: {lost} lost at {when}, with no VM stall before it")
            unexplained += 1
    return 1 if unexplained else 0


if __name__ == "__main__":
    sys.exit(main())
