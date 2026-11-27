"""Stands in for myna-bergamot: says ready, then upper-cases each line.

A line "SLEEP" answers after two seconds, "DIE" exits, "SPLIT" answers in two
writes, "TWO" answers with two lines at once, "BADBYTES" with bytes that are
not UTF-8. "NOREADY" in argv makes it start without saying ready, "SLOWSTART"
makes it take 0.4 s to.
"""

import os
import sys
import time

if "SLOWSTART" in sys.argv:
    time.sleep(0.4)
if "NOREADY" not in sys.argv:
    print("ready", flush=True)
for line in sys.stdin:
    line = line.rstrip("\n")
    if line == "SLEEP":
        time.sleep(2)
    elif line == "DIE":
        sys.exit(3)
    elif line == "SPLIT":
        os.write(1, b"AB")
        time.sleep(0.3)
        os.write(1, b"CD\n")
        continue
    elif line == "TWO":
        os.write(1, b"one\ntwo\n")
        continue
    elif line == "BADBYTES":
        os.write(1, b"\xff\n")
        continue
    print(line.upper(), flush=True)
