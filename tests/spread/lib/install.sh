#!/bin/bash
# install_snap — install a (prebuilt, --dangerous) snap, tolerating the
# snapd-restart-during-install race AND a snapd self-refresh that has not yet
# finished when the next install starts.
#
# Two failure modes this works around:
#
# 1. `snap install` loses its connection with
#      error: cannot communicate with server: Get .../v2/changes/N: EOF
#    when snapd restarts mid-install (wiring a user daemon, the gnome
#    extension, …). snapd finishes the pending install once it is back, so
#    the synchronous return is not authoritative — retry and verify via
#    `snap list`.
#
# 2. The previous install triggered a snapd self-refresh ("Waiting for
#    automatic snapd restart") that hasn't settled when the next install
#    begins. snapd's REST API answers, but the apply loop is busy and
#    `snap install` blocks indefinitely until spread's kill-timeout. Holding
#    refreshes before the test and waiting for snapd to be idle (no `Doing`
#    changes) before each attempt prevents the hang; `timeout` makes any
#    residual hang a fast error we can retry from.
#
# Usage:
#   install_snap <snap.snap> [extra-args...]
# e.g. install_snap myna-whisper_*.snap myna-whisper+model-tiny.comp
# The FIRST argument's snap name (basename up to the first '_') is what we
# verify with `snap list`.

# Wait until snapd has no changes in the `Doing` state. A snapd self-refresh
# shows up as a Doing change on the snapd snap, and API calls block while
# it is applying.
wait_snapd_idle() {
    local i
    for i in $(seq 1 60); do
        if ! snap changes --format=json 2>/dev/null | grep -q '"status":"Doing"'; then
            return 0
        fi
        sleep 5
    done
    return 0
}

install_snap() {
    local snap="$1"; shift
    local name
    name=$(basename "$snap"); name=${name%%_*}
    local i
    for i in $(seq 1 8); do
        wait_snapd_idle
        if snap list "$name" >/dev/null 2>&1; then
            echo "install_snap: $name already installed (attempt $i)"
            return 0
        fi
        # timeout so a hung install becomes an error we can retry from,
        # instead of a 15-minute spread kill-timeout.
        if timeout 300 snap install --dangerous "$snap" "$@" 2>"/tmp/install-$name.err"; then
            return 0
        fi
        echo "install_snap: $name attempt $i failed:" >&2
        cat "/tmp/install-$name.err" >&2
        # Wait for snapd to be reachable and idle again; the pending install
        # may have completed on its own.
        local j
        for j in $(seq 1 30); do
            snap list >/dev/null 2>&1 && break
            sleep 2
        done
    done
    echo "install_snap: giving up on $name" >&2
    return 1
}