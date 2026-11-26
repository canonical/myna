#!/usr/bin/env bash
# Provision a desktop LXD VM (GNOME, or --desktop xubuntu|kde) for
# the Myna Settings e2e suites, from the release's Ubuntu cloud image. Ends
# with the VM stopped and snapshots that mirror the states the suites
# declare:
#
#   bare             desktop up, no myna snaps, no user-daemons flag
#   components-only  flag on, myna + myna-parakeet installed from latest/edge,
#                    backend not connected
#   installed        components-only plus myna-whisper (a second backend to
#                    switch to), myna:backend connected to parakeet and the
#                    daemon restarted; on Plasma also IBus as KWin's input
#                    method, as onboarding leaves it
#   localized        installed plus the Swedish language pack, which
#                    translates snap's own output, and the Simplified
#                    Chinese one, whose speakers get another model
#   dictation        localized plus the dictation suite's rig: a virtual
#                    PipeWire microphone, GTK4 for the field fixture, and the
#                    fake backend snap (a scripted transcript) connected to
#                    myna:backend. Needs the snap built: `make snap-fake`, or
#                    E2E_FAKE_SNAP=PATH; without it the stage is skipped.
#   chains           dictation plus the French, Farsi and Simplified Chinese
#                    language packs (Settings runs in each) and the fake
#                    snap installed again, so a dictation snapshot taken with
#                    an older one still hears a given transcript.
#
# run-suite.sh copies one per suite and leaves the VM itself untouched;
# reprovision on a new edge revision or a stale image with --force.
#
# Usage: provision.sh --release noble|resolute|stonking [--desktop gnome|xubuntu|kde]
#        [--force] [--keep-stages]
#   --keep-stages  keep the six stage snapshots as well as the three a suite
#                  runs from: they are this script's resume cache, and on a
#                  copy-on-write pool they cost their deltas. Delete by hand.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$HERE/lib.sh"

RELEASE='' FORCE='' KEEP_STAGES=''
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --desktop) E2E_DESKTOP=$2; shift 2 ;;
        --force) FORCE=1; shift ;;
        --keep-stages) KEEP_STAGES=1; shift ;;
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
        -c limits.cpu="$E2E_VM_CPUS" -c limits.memory="$E2E_VM_MEMORY" -d root,size=20GiB \
        -c cloud-init.user-data="$(user_data)"
fi
# Each of the three states a suite runs from is built only when missing, so a
# base can be completed (an existing one gains `seeded`) without booting the
# stages again.
if snapshots "$VM" | grep -qx ready; then
    echo "== $VM: ready is there, no stages to run"
    STAGES=0
else
    STAGES=1
fi

# The display manager only starts from the next boot, the first stage's. A
# run that died before that stage finishes the first boot here.
if [ "$STAGES" = 1 ] && ! snapshots "$VM" | grep -qx bare; then
    lxc start "$VM" 2>/dev/null || true
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
    snapd_cache_save "$VM"
    stop_vm "$VM"
    lxc snapshot "$VM" "$name"
}

[ "$STAGES" = 0 ] || stage bare true
[ "$STAGES" = 0 ] || stage components-only '
    sudo snap set system experimental.user-daemons=true
    sudo snap install --edge myna
    sudo snap install --edge myna-parakeet'
# What Settings runs to make IBus KWin's input method; Xfce and GNOME have one.
IM_ON=''
[ "$E2E_DESKTOP" != kde ] || IM_ON='timeout 60 ibus start --type kde-wayland'
[ "$STAGES" = 0 ] || stage installed "
    sudo snap install --edge myna-whisper
    sudo snap connect myna:backend myna-parakeet:provider
    systemctl --user restart snap.myna.myna.service
    systemctl --user is-active snap.myna.myna.service
    $IM_ON"
[ "$STAGES" = 0 ] || stage localized '
    sudo apt-get update -q
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q \
        language-pack-sv-base language-pack-zh-hans-base'

# The fake backend is a snap built from fake-snap/ (snapcraft, a few minutes),
# so provisioning does not build it: the newest build in the tree, or
# E2E_FAKE_SNAP.
FAKE_SNAP=${E2E_FAKE_SNAP:-$(find "$HERE/../../fake-snap" -maxdepth 1 -name 'myna-fake-backend_*_amd64.snap' -printf '%T@ %p\n' | sort -nr | sed -n '1s/^[^ ]* //p')}
push_fake_snap() { lxc file push --uid 1000 --gid 1000 "$FAKE_SNAP" "$VM/home/ubuntu/myna-fake-backend.snap"; }
if [ -n "$FAKE_SNAP" ]; then
    [ "$STAGES" = 0 ] || stage dictation '
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
    # Linger starts PipeWire before the login session, when RTKit grants it
    # nothing, so its graph often ran SCHED_OTHER and dropped audio. The
    # group makes it realtime through the rlimits PipeWire ships instead.
    sudo usermod -aG pipewire ubuntu
    systemctl --user restart pipewire pipewire-pulse wireplumber
    sudo snap install --dangerous myna-fake-backend.snap
    rm myna-fake-backend.snap
    sudo snap disconnect myna:backend
    sudo snap connect myna:backend myna-fake-backend:provider
    systemctl --user restart snap.myna.myna.service
    systemctl --user is-active snap.myna.myna.service' push_fake_snap
    [ "$STAGES" = 0 ] || stage chains '
    sudo apt-get update -qq
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q \
        language-pack-fr-base language-pack-fa-base language-pack-zh-hans-base
    # The packs ship translations, not the locales: without these setlocale
    # fails and a GTK app stays in English whatever LANG says.
    sudo locale-gen fr_FR.UTF-8 fa_IR.UTF-8 zh_CN.UTF-8
    sudo snap install --dangerous myna-fake-backend.snap
    rm myna-fake-backend.snap
    snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"
    systemctl --user restart snap.myna.myna.service
    systemctl --user is-active snap.myna.myna.service' push_fake_snap
else
    echo "== no fake backend snap (make snap-fake, or E2E_FAKE_SNAP): skipping stage dictation"
fi

# The states a suite runs from. `bare` is a machine with no Myna (onboarding
# installs onto it); `ready` has the snaps, the locales and the fake backend
# but no text components, which is what an install-path suite needs; `seeded`
# has the components too, so a page or dictation suite pays no store time.
# The stage snapshots above exist so a failed provision can resume; they are
# not for suites. On `dir` each is a full 20 GiB disk, so three provisioned
# VMs once took 460 GB of snapshots, which is what a copy-on-write pool
# (E2E_POOL) is for.
[ "$STAGES" = 0 ] || lxc snapshot "$VM" ready
if snapshots "$VM" | grep -qx seeded; then
    echo "== $VM: seeded is there, no components to install"
else
    echo "== stage seeded: the text components the page and dictation suites use"
    lxc start "$VM"
    wait_ready "$VM"
    wait_snapd_idle "$VM"
    on_vm "$VM" 'set -e
sudo snap install --edge myna-text+model-disfluency-en
sudo snap install --edge myna-translation+model-translate-en-fr+model-translate-fr-en+model-translate-fa-en'
    wait_snapd_idle "$VM"
    snapd_cache_save "$VM"
    stop_vm "$VM"
    lxc snapshot "$VM" seeded
fi
if [ -z "$KEEP_STAGES" ]; then
    for name in $(snapshots "$VM"); do
        case $name in
            bare|ready|seeded) ;;
            *) lxc delete "$VM/$name" ;;
        esac
    done
fi

echo "== $VM provisioned; snapshots:"
snapshots "$VM"
