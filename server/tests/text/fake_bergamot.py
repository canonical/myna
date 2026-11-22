"""Stands in for myna-bergamot: says ready, then upper-cases each line.

A line "SLEEP" answers after two seconds, "DIE" exits, "NOREADY" in argv
makes it start without saying ready.
"""

import sys
import time

if "NOREADY" not in sys.argv:
    print("ready", flush=True)
for line in sys.stdin:
    line = line.rstrip("\n")
    if line == "SLEEP":
        time.sleep(2)
    elif line == "DIE":
        sys.exit(3)
    print(line.upper(), flush=True)
