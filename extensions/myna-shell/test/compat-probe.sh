#!/bin/bash
# compat-probe.sh - run test/compat-probe.js against the Shell's own typelibs
# (2026-08-27 Shell 46 backport).
#
#     test/compat-probe.sh               (from extensions/myna-shell/)
#
# exits 0 when shellCompat.js's capability detection agrees with the St and
# Meta installed here, 1 when it does not, 77 when there is nothing to judge
# - the same contract as gpu-probe.sh, which it sits beside for the same
# reason: the thing being checked is introspection data, and no amount of
# headless unit testing can substitute for the real typelib.
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if ! command -v gjs >/dev/null 2>&1; then
    echo "compat-probe: no gjs; skipping" >&2
    exit 77
fi

. "$HERE/shell-typelibs.sh"
if ! shell_typelibs_export; then
    echo "compat-probe: no mutter/gnome-shell typelibs found; skipping" >&2
    exit 77
fi

gjs -m "$HERE/compat-probe.js"
