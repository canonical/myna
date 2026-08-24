#!/bin/bash
# run-suite.sh - every extension test, in one entrypoint.
#
#     test/run-suite.sh                  (from extensions/myna-shell/)
#
# exits 0 when everything that could run passed, non-zero otherwise.
#
# Ordered by what each suite needs: the pure GJS contract tests need nothing,
# gpu-probe.sh needs mutter's typelibs, entrance-visual.sh needs a Shell it
# can start. The last two exit 77 when they cannot judge; that is a skip, and
# their headers say where each draws the line.
#
# One entrypoint because three callers run this same list - the myna-shell
# workshop's `gjs-test` action, test/next-shell.sh, and anyone in a checkout.
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cd "$(dirname "$HERE")" || exit 1

for t in test/*.test.js; do
    gjs -m "$t" || exit 1
done

# `cmd; rc=$?` would not survive a caller running us under `set -e`: a 77
# aborts at the call itself, before the assignment.
for check in test/gpu-probe.sh test/entrance-visual.sh; do
    rc=0
    "$check" || rc=$?
    [ "$rc" -eq 0 ] || [ "$rc" -eq 77 ] || exit "$rc"
done
