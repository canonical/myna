#!/usr/bin/env bash
# Provision a desktop LXD VM (GNOME, or Xubuntu with --desktop xubuntu) for
# the Myna Settings e2e suites, from the release's Ubuntu cloud image. Ends
# with the VM stopped and snapshots that mirror the states the suites
# declare:
#
#   bare             desktop up, no myna snaps, no user-daemons flag
#   components-only  flag on, myna + myna-parakeet installed from latest/edge,
#                    backend not connected
#   installed        components-only plus myna-whisper (a second backend to
#                    switch to), myna:backend connected to parakeet and the
#                    daemon restarted
#   dictation        installed plus the dictation suite's rig: a virtual
#                    PipeWire microphone, GTK4 for the field fixture, and the
#                    fake backend snap (a scripted transcript) connected to
#                    myna:backend. Needs the snap built: `make snap-fake`, or
#                    E2E_FAKE_SNAP=PATH; without it the stage is skipped.
#
# run-suite.sh copies one per suite and leaves the VM itself untouched;
# reprovision on a new edge revision or a stale image with --force.
#
# Usage: provision.sh --release noble|resolute|stonking [--desktop gnome|xubuntu] [--force]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$HERE/lib.sh"

RELEASE='' FORCE=''
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --desktop) E2E_DESKTOP=$2; shift 2 ;;
        --force) FORCE=1; shift ;;
        -h|--help) sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
check_release "$RELEASE"
check_desktop
VM=$(vm_name "$RELEASE")

[ -z "$FORCE" ] || lxc delete --force "$VM" 2>/dev/null || true

if ! lxc info "$VM" >/dev/null 2>&1; then
    echo "== $VM: first boot installs the desktop (cloud-init)"
    # E2E_POOL: a copy-on-write pool (zfs, btrfs) when the default is `dir`,
    # where every per-suite copy is a full copy of the disk.
    lxc init "$(image "$RELEASE")" "$VM" --vm ${E2E_POOL:+-s "$E2E_POOL"} \
        -c limits.cpu=4 -c limits.memory=6GiB -d root,size=20GiB \
        -c cloud-init.user-data="$(user_data)"
    lxc start "$VM"
    # The display manager only starts from the next boot, the first stage's.
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
    local name=$1 script=$2 prepare=${3:-}
    snapshots "$VM" | grep -qx "$name" && return 0
    echo "== stage $name"
    lxc start "$VM"
    wait_ready "$VM"
    wait_snapd_idle "$VM"
    [ -z "$prepare" ] || "$prepare"
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

# The fake backend is a snap built from fake-snap/ (snapcraft, a few minutes),
# so provisioning does not build it: the newest build in the tree, or
# E2E_FAKE_SNAP.
FAKE_SNAP=${E2E_FAKE_SNAP:-$(find "$HERE/../../fake-snap" -maxdepth 1 -name 'myna-fake-backend_*_amd64.snap' -printf '%T@ %p\n' | sort -nr | sed -n '1s/^[^ ]* //p')}
push_fake_snap() { lxc file push --uid 1000 --gid 1000 "$FAKE_SNAP" "$VM/home/ubuntu/myna-fake-backend.snap"; }
if [ -n "$FAKE_SNAP" ]; then
    stage dictation '
    sudo apt-get update -qq
    sudo apt-get install -y gir1.2-gtk-4.0
    # A null sink whose monitor, through a loopback, is a source of the class
    # a real microphone has: the default input, so the daemon captures what
    # the suite plays to the sink. Neither node pauses when idle (a paused
    # virtual node hands out an empty stream).
    sudo install -D -m644 /dev/stdin /etc/pipewire/pipewire.conf.d/50-myna-e2e-mic.conf <<CONF
context.objects = [
  { factory = adapter
    args = {
      factory.name                    = support.null-audio-sink
      node.name                       = "myna-e2e-speaker"
      node.description                = "Myna e2e speaker"
      media.class                     = Audio/Sink
      audio.position                  = [ FL FR ]
      node.pause-on-idle              = false
      session.suspend-timeout-seconds = 0
    }
  }
]
context.modules = [
  { name = libpipewire-module-loopback
    args = {
      capture.props = {
        node.name           = "myna-e2e-mic.input"
        node.passive        = true
        target.object       = "myna-e2e-speaker"
        stream.capture.sink = true
      }
      playback.props = {
        node.name                       = "myna-e2e-mic"
        node.description                = "Myna e2e microphone"
        media.class                     = Audio/Source
        audio.position                  = [ FL FR ]
        node.pause-on-idle              = false
        session.suspend-timeout-seconds = 0
      }
    }
  }
]
CONF
    systemctl --user restart pipewire pipewire-pulse wireplumber
    sudo snap install --dangerous myna-fake-backend.snap
    rm myna-fake-backend.snap
    sudo snap disconnect myna:backend
    sudo snap connect myna:backend myna-fake-backend:provider
    systemctl --user restart snap.myna.myna.service
    systemctl --user is-active snap.myna.myna.service' push_fake_snap
else
    echo "== no fake backend snap (make snap-fake, or E2E_FAKE_SNAP): skipping stage dictation"
fi

echo "== $VM provisioned; snapshots:"
snapshots "$VM"
