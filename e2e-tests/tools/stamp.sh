#!/bin/bash
# stamp.sh - prefix every line of a suite's output with its wall clock time and
# the seconds since the first line, so a run's breakdown can be read back
# afterwards (tools/breakdown.py). Reads stdin, writes stdout.
#
# A suite prints its steps as `-- N. name` and its assertions as `PASS:`/`FAIL:`;
# the stamps are what say how long each step took.
set -uo pipefail
exec awk '
{
    now = systime()
    if (!start) { start = now }
    printf "%s %6ds  %s\n", strftime("%H:%M:%S", now, 1), now - start, $0
    fflush()
}'
