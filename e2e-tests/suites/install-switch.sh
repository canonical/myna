#!/usr/bin/env bash
# snapshot: installed
# Choosing a model snapd is still installing (issue #51): its slot lists
# before its model has arrived, and a switch to it used to disconnect
# Parakeet, then fail the connect on snapd's "changes in progress", leaving
# no model at all. A drop-in holds the server's start, the install's last
# step, until the suite releases it: a cached snap installs in seconds.
# Settings must show it installing, keep Parakeet connected whatever is
# clicked, and let it be chosen once done.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

S=shots/install-switch

assert_on "precondition: backend is parakeet" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'
on 'sudo snap remove --purge myna-whisper' >/dev/null \
    || { echo "could not remove whisper" >&2; exit 1; }

HOLD=/etc/systemd/system/snap.myna-whisper.server.service.d
RELEASE=/run/myna-e2e-install-released
on "sudo mkdir -p $HOLD && printf '%s\n' '[Service]' TimeoutStartSec=infinity \
    'ExecStartPre=/bin/sh -c \"until [ -e $RELEASE ]; do sleep 0.2; done\"' \
    | sudo tee $HOLD/e2e-hold.conf" >/dev/null \
    || { echo "could not hold the server's start" >&2; exit 1; }
on 'sudo snap install --edge --no-wait myna-whisper' >/dev/null \
    || { echo "could not start the install" >&2; exit 1; }
# Held: the slot lists, the install is still Doing. A cold cache downloads.
# shellcheck disable=SC2016 # expands in the VM
on 'for _ in $(seq 600); do
    [ "$(systemctl show -P ActiveState snap.myna-whisper.server.service)" = activating ] \
        && snap list myna-whisper && snap changes | grep -q "Doing.*Install \"myna-whisper\"" && exit 0
    sleep 0.5
done; exit 1' >/dev/null \
    || { echo "whisper's install never reached the hold" >&2; exit 1; }

# The click is a negative check: the wait is how long a switch's disconnect
# took to land when the row still took it.
shot --polkit allow --monitor install-switch \
    waitfor:General "waitfor:Whisper@radio button" "waitfor:Installing" \
    shot:$S/01-installing.png \
    click:Whisper wait:5 shot:$S/02-clicked.png \
    || { echo "Whisper never showed installing" >&2; FAILED=1; }
assert_on "parakeet still connected while whisper installs" \
    'snap connections myna | grep -q "myna:backend.*myna-parakeet:provider"'
assert_on "whisper not connected while it installs" \
    '! snap connections myna | grep -q "myna:backend.*myna-whisper:provider"'

on "sudo touch $RELEASE"
on 'sudo snap watch --last=install' >/dev/null \
    || { echo "the install did not finish" >&2; exit 1; }

# Once done it is a model like any other.
shot --polkit allow \
    waitfor:General \
    click:Whisper "checked:Whisper@radio button" \
    "$(poll 'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"')" \
    shot:$S/03-switched.png \
    || { echo "switch after install failed" >&2; FAILED=1; }
assert_on "backend connected to whisper once installed" \
    'snap connections myna | grep -q "myna:backend.*myna-whisper:provider"'
suite_status
