#!/usr/bin/env bash
# Provision a GNOME desktop LXD VM for the Myna Settings e2e suites, from
# the release's Ubuntu cloud image. Ends with the VM stopped and three
# snapshots that mirror the states the suites declare:
#
#   bare             desktop up, no myna snaps, no user-daemons flag
#   components-only  flag on, myna + myna-parakeet installed from latest/edge,
#                    backend not connected
#   installed        components-only plus myna-whisper (a second backend to
#                    switch to), myna:backend connected to parakeet and the
#                    daemon restarted
#
# run-suite.sh copies one per suite and leaves the VM itself untouched; reprovision on a new edge revision
# or a stale image with --force.
#
# Usage: provision.sh --release noble|resolute|stonking [--force]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$HERE/lib.sh"

RELEASE='' FORCE=''
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --force) FORCE=1; shift ;;
        -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
check_release "$RELEASE"
VM=$(vm_name "$RELEASE")

[ -z "$FORCE" ] || lxc delete --force "$VM" 2>/dev/null || true

if ! lxc info "$VM" >/dev/null 2>&1; then
    echo "== $VM: first boot installs the desktop (cloud-init)"
    lxc init "$(image "$RELEASE")" "$VM" --vm \
        -c limits.cpu=4 -c limits.memory=6GiB -d root,size=20GiB \
        -c cloud-init.user-data="$(cat "$HERE/user-data")"
    lxc start "$VM"
    # gdm only starts from the next boot, the first stage's.
    wait_booted "$VM"
    # A suite's copy is a new instance to cloud-init, which would run the
    # whole user-data again.
    lxc exec "$VM" -- touch /etc/cloud/cloud-init.disabled
    stop_vm "$VM"
fi

# A failed earlier run may have left it up.
lxc stop --force "$VM" 2>/dev/null || true

# One stage: boot, run the stage's commands, stop, snapshot.
stage() {
    local name=$1 script=$2
    snapshots "$VM" | grep -qx "$name" && return 0
    echo "== stage $name"
    lxc start "$VM"
    wait_ready "$VM"
    wait_snapd_idle "$VM"
    on_vm "$VM" "set -e; $script"
    wait_snapd_idle "$VM"
    stop_vm "$VM"
    lxc snapshot "$VM" "$name"
}

stage bare true
stage components-only '
    sudo snap set system experimental.user-daemons=true
    sudo snap install --edge myna
    sudo snap install --edge myna-parakeet'
stage installed '
    sudo snap install --edge myna-whisper
    sudo snap connect myna:backend myna-parakeet:provider
    systemctl --user restart snap.myna.myna.service
    systemctl --user is-active snap.myna.myna.service'

echo "== $VM provisioned; snapshots:"
snapshots "$VM"
