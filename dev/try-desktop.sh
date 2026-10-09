#!/usr/bin/env bash
# A desktop VM running THIS tree's Myna, to try by hand (not a test).
#
# Usage: dev/try-desktop.sh --desktop xubuntu|gnome --release noble|resolute ACTION
#   (or: make try-desktop DESKTOP=xubuntu RELEASE=noble ACTION=up)
#
#   up       create myna-try-<desktop>-<release> from the e2e VM's `installed`
#            snapshot (provisioned first when missing), install the myna snap
#            and the myna-config deb built from HEAD, snapshot `fresh`, start
#   update   rebuild what HEAD changed, install it over the running VM, restart
#            the daemon and HUD host; settings and files are kept
#   console  start the VM if needed and open its desktop (SPICE, remote-viewer)
#   audio    point the VM at the host's audio again (after the host's
#            pipewire-pulse restarted), or at other devices: TRY_HOST_SOURCE
#            and TRY_HOST_SINK name host pulse devices (default: the host's)
#   reset    restore `fresh` (the artifacts of the last `up`) and start
#   down     stop the VM (it is kept) and stop sharing the host's audio
#   delete   remove the VM, keeping the e2e VMs and the build caches
#   status   state, installed versions
#
# `installed` rather than `dictation`: it has the real parakeet backend and no
# virtual microphone to hide the real one. The VM is a copy, so the e2e VMs
# are never touched. Artifacts come from HEAD, not the working tree: the snap
# from a clean clone (e2e-tests/.run/try/src; the first build is a full
# snapcraft build, later ones are incremental) and the deb through sbuild.
# The deb gets PPA=1 versioning only when HEAD is on origin/main, which
# build-source.sh insists on; a branch commit can never be the PPA's, so the
# plain version cannot collide with it.
#
# Audio: qemu in the LXD snap has no pipewire or pulse audiodev, and the SPICE
# viewer `lxc console` starts runs under the lxd snap's confinement without
# access to the host's sound server. So the host's pipewire-pulse listens on
# the VM's bridge address (TCP, port 4714, loaded with pactl while a try VM
# runs) and the VM's pipewire tunnels a "Host microphone" source and "Host
# speakers" sink to it, the defaults there. Trade-off: while it runs, any other
# guest on that bridge can open the host's audio too (pipewire-pulse has no
# address ACL); nothing is exposed beyond the bridge and `down` unloads it.
#
# Environment: TRY_MEMORY (4GiB), TRY_CPU (4), TRY_MIN_MEM_GIB (6) and
# TRY_MIN_DISK_GIB (15) are the available RAM and free disk below which
# nothing is started or built.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/.." && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$REPO/e2e-tests/vm/lib.sh"

TRY_DIR=$RUN_DIR/try
MEMORY=${TRY_MEMORY:-4GiB}
CPU=${TRY_CPU:-4}
MIN_MEM=${TRY_MIN_MEM_GIB:-6}
MIN_DISK=${TRY_MIN_DISK_GIB:-15}

PULSE_PORT=4714

die() { echo "try-desktop: $*" >&2; exit 1; }

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; }

DESK=xubuntu REL=noble ACTION=''
while [ $# -gt 0 ]; do
    case $1 in
        --desktop) DESK=$2; shift 2 ;;
        --release) REL=$2; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        up|update|console|audio|reset|down|delete|status) ACTION=$1; shift ;;
        *) echo "unknown argument $1 (try --help)" >&2; exit 2 ;;
    esac
done
[ -n "$ACTION" ] || { usage >&2; exit 2; }
E2E_DESKTOP=$DESK
check_desktop
check_release "$REL"
BASE=$(vm_name "$REL")
TRY=myna-try-$DESK-$REL
HEAD_SHA=$(git -C "$REPO" rev-parse HEAD)

exists() { lxc info "$TRY" >/dev/null 2>&1; }
running() { [ "$(lxc info "$TRY" 2>/dev/null | sed -n 's/^Status: //p')" = RUNNING ]; }

# Refuse to start or build with too little headroom: the host has run out of
# memory before.
guard() {
    local mem disk
    mem=$(free -g | awk '/^Mem:/ {print $7}')
    disk=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
    [ "$mem" -ge "$MIN_MEM" ] \
        || die "only $mem GiB of RAM is available, $MIN_MEM needed: stop other VMs or builds (TRY_MIN_MEM_GIB lowers the bar)"
    [ "$disk" -ge "$MIN_DISK" ] \
        || die "only $disk GiB free on /, $MIN_DISK needed: free some space (TRY_MIN_DISK_GIB lowers the bar)"
}

# The address the host's audio is offered on: the bridge the VM is on.
bridge_ip() {
    local net
    net=$(lxc query "/1.0/instances/$TRY" | jq -r '[.expanded_devices[] | select(.type == "nic") | .network][0]')
    lxc network get "$net" ipv4.address | cut -d/ -f1
}

host_audio_up() {
    command -v pactl >/dev/null || { echo "== no pactl: the VM gets no host audio" >&2; return 0; }
    local ip; ip=$(bridge_ip)
    ss -ltn | grep -q "$ip:$PULSE_PORT " \
        || pactl load-module module-native-protocol-tcp port=$PULSE_PORT listen="$ip" >/dev/null
}

# Other try VMs may still use it.
host_audio_down() {
    lxc list myna-try- -f csv -c ns | grep -q RUNNING && return 0
    command -v pactl >/dev/null || return 0
    pactl list short modules | awk -v p="port=$PULSE_PORT" '$2 == "module-native-protocol-tcp" && index($0, p) {print $1}' \
        | while read -r id; do pactl unload-module "$id"; done
}

# The VM's pipewire: a source and a sink tunnelled to the host's.
audio_setup() {
    local ip; ip=$(bridge_ip)
    local tmp; tmp=$(mktemp)
    cat > "$tmp" <<CONF
# Written by dev/try-desktop.sh: the host's audio over pulse TCP.
context.modules = [
  { name = libpipewire-module-pulse-tunnel
    args = {
      tunnel.mode = source
      pulse.server.address = "tcp:$ip:$PULSE_PORT"
${TRY_HOST_SOURCE:+      target.object = \"$TRY_HOST_SOURCE\"
}      node.name = "myna-host-mic"
      node.description = "Host microphone"
      priority.session = 3000
    }
    flags = [ nofail ]
  }
  { name = libpipewire-module-pulse-tunnel
    args = {
      tunnel.mode = sink
      pulse.server.address = "tcp:$ip:$PULSE_PORT"
${TRY_HOST_SINK:+      target.object = \"$TRY_HOST_SINK\"
}      node.name = "myna-host-speaker"
      node.description = "Host speakers"
      priority.session = 3000
    }
    flags = [ nofail ]
  }
]
CONF
    on_vm "$TRY" 'mkdir -p ~/.config/pipewire/pipewire.conf.d'
    lxc file push --uid 1000 --gid 1000 "$tmp" "$TRY/home/ubuntu/.config/pipewire/pipewire.conf.d/60-myna-host-audio.conf"
    rm -f "$tmp"
}

start_vm() {
    running && return 0
    guard
    host_audio_up
    lxc start "$TRY"
    wait_ready "$TRY"
}

# The snap, from a clean clone of HEAD. Cached by commit.
build_snap() {
    local src=$TRY_DIR/src out=$TRY_DIR/myna.snap
    if [ -f "$out" ] && [ "$(cat "$out.head" 2>/dev/null)" = "$HEAD_SHA" ]; then
        echo "== snap: cached for ${HEAD_SHA:0:8}"
        return 0
    fi
    guard
    echo "== snap: building ${HEAD_SHA:0:8} (the first build is slow)"
    mkdir -p "$TRY_DIR"
    [ -d "$src/.git" ] || git clone -q --no-hardlinks "$REPO" "$src"
    git -C "$src" fetch -q --tags "$REPO" HEAD
    git -C "$src" checkout -q --detach -f "$HEAD_SHA"
    git -C "$src" clean -fdxq
    (cd "$src/myna-snap" && ./dev/prepare.sh >/dev/null && snapcraft pack)
    cp "$(find "$src/myna-snap" -maxdepth 1 -name 'myna_*_amd64.snap' -printf '%T@ %p\n' | sort -nr | sed -n '1s/^[^ ]* //p')" "$out"
    echo "$HEAD_SHA" > "$out.head"
}

# The deb, sbuilt for the release. Cached by commit.
build_deb() {
    local stage=$TRY_DIR/deb-$REL out ppa=()
    out=$stage/myna-config.deb
    if [ -f "$out" ] && [ "$(cat "$stage.head" 2>/dev/null)" = "$HEAD_SHA" ]; then
        echo "== deb: cached for ${HEAD_SHA:0:8}"
        return 0
    fi
    guard
    if git -C "$REPO" merge-base --is-ancestor HEAD origin/main 2>/dev/null; then
        ppa=(PPA=1)
    else
        echo "== deb: HEAD is not on origin/main, so no ~ppa1 version (the PPA cannot publish this commit)"
    fi
    echo "== deb: building ${HEAD_SHA:0:8} for $REL"
    local work=$TRY_DIR/deb-work-$REL
    env SERIES="$REL" "${ppa[@]}" "$REPO/myna-config-deb/build-source.sh" "$work" >/dev/null
    # sbuild cleans the source on the host first, which needs the series'
    # toolchain; /tmp is a tmpfs here and a debuginfo build fills it.
    # shellcheck disable=SC2086 # SBUILD_ARGS is a word list, as for `make build-deb`
    (cd "$work"/myna-config-*/ && TMPDIR=/var/tmp sbuild --no-clean-source --build-dir="$work" ${SBUILD_ARGS:-})
    rm -rf "$stage"; mkdir -p "$stage"
    cp "$work"/myna-config_*_amd64.deb "$out"
    echo "$HEAD_SHA" > "$stage.head"
    rm -rf "$work"
}

connections() { lxc exec "$TRY" -- snap connections "$1" | awk 'NR > 1 && $2 != "-" && $3 != "-" {print $2, $3}' | sort; }

# Push the built artifacts into the running VM and restart what runs them.
install_artifacts() {
    local before after plug slot
    wait_snapd_idle "$TRY"
    lxc file push "$TRY_DIR/myna.snap" "$TRY/root/myna.snap"
    lxc file push "$TRY_DIR/deb-$REL/myna-config.deb" "$TRY/root/myna-config.deb"

    before=$(connections myna)
    lxc exec "$TRY" -- snap install --dangerous /root/myna.snap
    # A sideloaded snap is granted what the store would grant by default, but
    # not the connections the store install had; put back what is missing.
    after=$(connections myna)
    while read -r plug slot; do
        [ -n "$plug" ] || continue
        grep -qxF "$plug $slot" <<<"$after" || lxc exec "$TRY" -- snap connect "$plug" "$slot"
    done <<<"$before"

    local log
    log=$(lxc exec "$TRY" -- env DEBIAN_FRONTEND=noninteractive bash -c \
        'apt-get update -qq && apt-get install -y -qq --allow-downgrades /root/myna-config.deb' 2>&1) \
        || { echo "$log" >&2; die "installing the deb failed"; }
    lxc exec "$TRY" -- rm -f /root/myna.snap /root/myna-config.deb
    lxc config set "$TRY" user.myna-try.head "$HEAD_SHA"
}

# Restart the daemon and, on Xfce, the HUD host (GNOME loads a new shell
# extension only at the next login).
restart_services() {
    # shellcheck disable=SC2016 # expands in the VM
    on_vm "$TRY" 'systemctl --user restart snap.myna.myna.service
for _ in $(seq 20); do systemctl --user is-active --quiet snap.myna.myna.service && exit 0; sleep 1; done
echo "daemon not active" >&2; exit 1'
    if [ "$DESK" = xubuntu ]; then
        # shellcheck disable=SC2016 # expands in the VM
        on_vm "$TRY" 'pkill -x myna-hud-host || true; pkill -x myna-hud || true
exe=$(sed -n "s/^Exec=//p" /etc/xdg/autostart/com.canonical.Myna.HudHost.desktop)
setsid -f $exe >/dev/null 2>&1 </dev/null'
    fi
}

report() {
    echo "== $TRY: $(lxc info "$TRY" | sed -n 's/^Status: //p'), built from $(lxc config get "$TRY" user.myna-try.head | cut -c1-8)"
    if running; then
        lxc exec "$TRY" -- snap list myna myna-parakeet 2>/dev/null | awk 'NR > 1 {print "   snap", $1, $2}'
        lxc exec "$TRY" -- dpkg-query -W myna-config | awk '{print "   deb", $1, $2}'
    fi
}

do_up() {
    if exists; then
        echo "== $TRY exists; starting it (update refreshes the artifacts, delete starts over)"
        start_vm; report; return
    fi
    guard
    snapshots "$BASE" 2>/dev/null | grep -qx installed || {
        echo "== $BASE has no installed snapshot: provisioning it first"
        "$REPO/e2e-tests/vm/provision.sh" --release "$REL" --desktop "$DESK"
    }
    build_snap
    build_deb
    echo "== creating $TRY from $BASE/installed"
    lxc copy "$BASE/installed" "$TRY" -c limits.memory="$MEMORY" -c limits.cpu="$CPU"
    start_vm
    audio_setup
    install_artifacts
    echo "== snapshotting fresh"
    stop_vm "$TRY"
    lxc snapshot "$TRY" fresh
    start_vm
    restart_services
    report
    echo "== next: dev/try-desktop.sh --desktop $DESK --release $REL console"
}

do_update() {
    exists || die "$TRY does not exist: run up"
    build_snap
    build_deb
    start_vm
    install_artifacts
    restart_services
    report
    [ "$DESK" != gnome ] || echo "== GNOME loads a changed shell extension at the next login"
}

do_console() {
    exists || die "$TRY does not exist: run up"
    command -v remote-viewer >/dev/null || die "remote-viewer is missing: sudo apt install virt-viewer"
    [ -n "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ] || die "no graphical session to open the window in"
    start_vm
    setsid -f lxc console "$TRY" --type=vga >/dev/null 2>&1 </dev/null
    echo "== console opened"
}

do_audio() {
    exists || die "$TRY does not exist: run up"
    start_vm
    host_audio_up
    audio_setup
    on_vm "$TRY" 'systemctl --user restart pipewire pipewire-pulse wireplumber'
    sleep 3
    on_vm "$TRY" 'wpctl status | sed -n "/^Audio/,/^Video/p" | grep -E "Host"'
}

do_reset() {
    exists || die "$TRY does not exist: run up"
    running && stop_vm "$TRY"
    lxc restore "$TRY" fresh
    start_vm
    restart_services
    report
    [ "$(lxc config get "$TRY" user.myna-try.head)" = "$HEAD_SHA" ] \
        || echo "== fresh predates HEAD (${HEAD_SHA:0:8}); update installs the current build"
}

case $ACTION in
    up) do_up ;;
    update) do_update ;;
    console) do_console ;;
    audio) do_audio ;;
    reset) do_reset ;;
    down) if running; then stop_vm "$TRY"; fi; host_audio_down; echo "== $TRY stopped" ;;
    delete) lxc delete --force "$TRY" 2>/dev/null || true; host_audio_down; echo "== $TRY deleted" ;;
    status) exists && report || echo "== $TRY does not exist" ;;
esac
