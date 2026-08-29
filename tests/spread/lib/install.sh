#!/bin/bash
# install_snap — install a (prebuilt, --dangerous) snap, tolerating the
# snapd-restart-during-install race.
#
# Symptom this works around: `snap install` can lose its connection with
#   error: cannot communicate with server: Get "http://localhost/v2/changes/N": EOF
# when snapd restarts mid-install (wiring a user daemon, the gnome extension,
# …). snapd finishes the pending install once it is back, so the synchronous
# return is not authoritative — retry and verify via `snap list`.
#
# Usage:
#   install_snap <snap.snap> [extra-args...]
# e.g. install_snap myna-whisper_*.snap myna-whisper+model-tiny.comp
# The FIRST argument's snap name (basename up to the first '_') is what we
# verify with `snap list`.

install_snap() {
    local snap="$1"; shift
    local name
    name=$(basename "$snap"); name=${name%%_*}
    local i j
    for i in $(seq 1 8); do
        if snap list "$name" >/dev/null 2>&1; then
            echo "install_snap: $name already installed (attempt $i)"
            return 0
        fi
        if snap install --dangerous "$snap" "$@" 2>"/tmp/install-$name.err"; then
            return 0
        fi
        echo "install_snap: $name attempt $i failed:" >&2
        cat "/tmp/install-$name.err" >&2
        # Wait for snapd to be reachable again; the pending install may have
        # completed on its own by the time it is back.
        for _j in $(seq 1 30); do
            snap list >/dev/null 2>&1 && break
            sleep 2
        done
    done
    echo "install_snap: giving up on $name" >&2
    return 1
}
