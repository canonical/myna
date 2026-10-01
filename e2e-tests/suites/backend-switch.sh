#!/usr/bin/env bash
# snapshot: installed
# Backend switching on General: with parakeet connected and whisper also
# installed, activating Whisper's radio raises snapd's polkit prompt; a
# dismissed prompt silently reverts the radio and changes nothing, an
# allowed one connects myna:backend to whisper and restarts the daemon.
set -uo pipefail
source "$SUITE_LIB"

P=backend-switch-$REL

if ! on 'snap list myna-whisper >/dev/null 2>&1'; then
    echo "== installing myna-whisper (second backend for the switch)"
    on 'sudo snap install --edge myna-whisper
for _ in $(seq 300); do
    [ -z "$(snap changes | awk "NR>1 && \\$NF != \"Done\" && \\$NF != \"Hold\"")" ] && exit 0
    sleep 5
done; exit 1' || { echo "whisper install failed" >&2; exit 1; }
fi
assert_ssh "precondition: backend is parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# A dismissed prompt reverts silently: the connection must not move.
shot --polkit cancel \
    wait:4 waitfor:General \
    click:Whisper wait:4 shot:"$P"-01-cancelled.png \
    || { echo "cancel run failed" >&2; exit 1; }
assert_ssh "cancel left backend on parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'

# An allowed prompt switches: connection moves, daemon comes back active.
shot --no-build --polkit allow --monitor "$P" \
    wait:4 waitfor:General \
    click:Whisper wait:10 shot:"$P"-02-switched.png \
    || { echo "allow run failed" >&2; exit 1; }
assert_ssh "backend connected to whisper" \
    'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"'
assert_ssh "daemon active after switch" \
    'systemctl --user is-active -q snap.myna.myna.service'
suite_status
