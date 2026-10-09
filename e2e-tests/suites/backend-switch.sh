#!/usr/bin/env bash
# snapshot: installed
# Backend switching on General: with parakeet connected and whisper also
# installed, activating Whisper's radio raises snapd's polkit prompt; a
# dismissed prompt silently reverts the radio and changes nothing, an
# allowed one connects myna:backend to whisper and restarts the daemon.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

P=backend-switch-$REL
S=shots/backend-switch

assert_on "precondition: backend is parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# A dismissed prompt reverts silently: the connection must not move.
shot --polkit cancel \
    waitfor:General "waitfor:Parakeet@radio button" shot:$S/01-general.png \
    click:Whisper "$PROMPT_DISMISSED" "checked:Parakeet@radio button" shot:$S/02-cancelled.png \
    || { echo "cancel run failed" >&2; exit 1; }
assert_on "cancel left backend on parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# An allowed prompt switches: connection moves, daemon comes back active.
shot --polkit allow --monitor "$P" \
    waitfor:General \
    click:Whisper "checked:Whisper@radio button" \
    "$(poll 'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"')" \
    shot:$S/03-switched.png \
    "tab:Diagnostics" "waittext:Diagnostic report=Onboarding:" shot:$S/04-diagnostics.png \
    || { echo "allow run failed" >&2; exit 1; }
assert_on "backend connected to whisper" \
    'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"'
assert_soon "daemon active after switch" \
    'systemctl --user is-active -q snap.myna.myna.service'
suite_status
