#!/bin/bash
# gpu-probe.sh - run test/gpu-probe.js against the Shell's own typelibs
# (feature 004-gnome-shell-indicator, 2026-08-21 GPU pass).
#
#     test/gpu-probe.sh                  (from extensions/myna-shell/)
#
# exits 0 when the GPU path's toolkit API checks out, 1 when it does not,
# 77 when there is nothing here to judge - the same contract as
# entrance-visual.sh.
#
# gpu-probe.js needs Clutter, Cogl and St, and none of them are on the
# default typelib search path: mutter and gnome-shell install private
# typelibs beside their own libraries. Finding them is all this wrapper
# does. It needs no display server; the probe only builds the snippet and
# inspects the effect class.
#
# The probe was documented as "run manually" against a hand-set path into a
# self-built GNOME tree, so nothing ran it on the Shell the workshop
# actually has - and nothing noticed that the CoglSnippet ShaderEffect API
# does not exist on Shell 50 until the extension was already broken there.
# A test that needs a path nobody has is a test that does not run.
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if ! command -v gjs >/dev/null 2>&1; then
    echo "gpu-probe: no gjs; skipping" >&2
    exit 77
fi

# shellcheck source-path=SCRIPTDIR source=shell-typelibs.sh
. "$HERE/shell-typelibs.sh"
if ! shell_typelibs_export; then
    echo "gpu-probe: no mutter/gnome-shell typelibs found; skipping" >&2
    exit 77
fi

gjs -m "$HERE/gpu-probe.js"
