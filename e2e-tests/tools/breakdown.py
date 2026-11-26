#!/usr/bin/python3
"""breakdown.py - read a stamped suite log and print where the time went.

Input is what tools/stamp.sh writes (`HH:MM:SS NNNNs  line`). Steps are the
suite's own headers (`-- N. name`), assertions its `PASS:`/`FAIL:` lines, so a
suite costs no bookkeeping: it already says what it is doing when.

Usage: breakdown.py LOG [LOG...]
"""

import re
import sys

STAMP = re.compile(r"^\d\d:\d\d:\d\d\s+(\d+)s\s\s(.*)$")
STEP = re.compile(r"^-- (\d+)\. (.*)$")

# A step is [text, start seconds, pass count, fail count].
STEP_TEXT, STEP_START, STEP_PASS, STEP_FAIL = range(4)


def read(path):
    steps = []
    current = None
    total = 0
    with open(path, encoding="utf-8", errors="replace") as log:
        for line in log:
            m = STAMP.match(line.rstrip("\n"))
            if not m:
                continue
            seconds, text = int(m.group(1)), m.group(2)
            total = max(total, seconds)
            step = STEP.match(text)
            if step:
                current = [step.group(2), seconds, 0, 0]
                steps.append(current)
            elif current is not None and text.startswith("PASS:"):
                current[STEP_PASS] += 1
            elif current is not None and text.startswith("FAIL:"):
                current[STEP_FAIL] += 1
    return steps, total


def report(path):
    steps, total = read(path)
    failed = sum(step[STEP_FAIL] for step in steps)
    name = path.rsplit("/", 1)[-1]
    print(f"{name}: {total // 60}m{total % 60:02d}s, {len(steps)} steps, {failed} failed")
    if steps:
        print(f"  {'step':>4}  {'time':>6}  {'pass':>4}  {'fail':>4}  name")
    for index, step in enumerate(steps):
        end = steps[index + 1][STEP_START] if index + 1 < len(steps) else total
        took = max(0, end - step[STEP_START])
        print(
            f"  {index:>4}  {took:>4}s  {step[STEP_PASS]:>4}  {step[STEP_FAIL]:>4}  {step[STEP_TEXT]}"
        )
    return 1 if failed else 0


def main(paths):
    status = 0
    for path in paths:
        status |= report(path)
    return status


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__.strip(), file=sys.stderr)
        raise SystemExit(2)
    raise SystemExit(main(sys.argv[1:]))
