#!/usr/bin/env bash
# fleet.sh - the e2e base VMs: which exist, what each holds, what it costs,
# and how to take one down.
#
# A base is one provisioned VM per (release, desktop), with two snapshots:
# `bare` (what onboarding installs onto) and `ready` (everything installed,
# what every other suite runs from). Suites do not run on the base: run-suite.sh
# copies it, runs, and deletes the copy. The bases are the warm cache and stay.
#
# Usage: fleet.sh list [--release R] [--desktop D]
#        fleet.sh delete --release R --desktop D [--force]
#
# Sizes come from lxc, so this needs no root: a `dir` base costs a full disk per
# snapshot, a copy-on-write one (E2E_POOL) only its deltas.
set -euo pipefail
# shellcheck source=vm/lib.sh
source "$(dirname "$0")/vm/lib.sh"

ACTION=${1:-list}
shift || true
RELEASE='' DESKTOP='' FORCE=''
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --desktop) DESKTOP=$2; shift 2 ;;
        --force) FORCE=1; shift ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done

names() {
    local release desktop
    for release in ${RELEASE:-$RELEASES}; do
        for desktop in ${DESKTOP:-$DESKTOPS}; do
            E2E_DESKTOP=$desktop vm_name "$release"
        done
    done
}

case $ACTION in
    list)
        printf '%-28s %-8s %-26s %s\n' VM STATE SNAPSHOTS SIZE
        for vm in $(names); do
            if ! lxc info "$vm" >/dev/null 2>&1; then
                printf '%-28s %-8s %-26s %s\n' "$vm" absent - -
                continue
            fi
            state=$(lxc list "$vm" --format csv -c s 2>/dev/null || echo '?')
            snaps=$(snapshots "$vm" | tr '\n' ',' | sed 's/,$//')
            size=$(lxc info "$vm" 2>/dev/null | awk '/^  Disk usage:/{getline; print $2; exit}')
            printf '%-28s %-8s %-26s %s\n' "$vm" "${state:-?}" "${snaps:--}" "${size:--}"
        done
        ;;
    delete)
        [ -n "$RELEASE" ] && [ -n "$DESKTOP" ] || {
            echo "delete needs --release and --desktop" >&2; exit 2
        }
        vm=$(E2E_DESKTOP=$DESKTOP vm_name "$RELEASE")
        lxc info "$vm" >/dev/null 2>&1 || { echo "$vm does not exist" >&2; exit 1; }
        # A VM with snapshots needs --force to go; say what it costs first.
        echo "== $vm: $(snapshots "$vm" | tr '\n' ',' | sed 's/,$//')"
        lxc delete --force "$vm"
        echo "== $vm deleted"
        ;;
    *)
        echo "usage: fleet.sh list|delete ..." >&2; exit 2 ;;
esac
