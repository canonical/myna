# shellcheck shell=bash
# Shared helpers for the e2e scripts. Source, do not execute.

# lxc reads stdin even when it has nothing to send; under ssh (Testflinger)
# that hangs the caller.
[ -t 0 ] || exec </dev/null

E2E_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export RUN_DIR=$E2E_ROOT/.run
RELEASES="noble resolute stonking"
DESKTOPS="gnome xubuntu"
# The guest's RAM. CI's runners take the 6GiB the desktops were sized for; a
# laptop that is also somebody's desktop sets E2E_VM_MEMORY=4GiB (it applies
# to every copy a suite runs in, whatever the VM was provisioned with).
export E2E_VM_MEMORY=${E2E_VM_MEMORY:-6GiB}
# Which desktop the VM runs; scripts take --desktop to override.
export E2E_DESKTOP=${E2E_DESKTOP:-gnome}

# The GNOME VM keeps its historical name so existing VMs and snapshots stay
# valid; every other desktop is named after itself, so they coexist.
vm_name() {
    case $E2E_DESKTOP in
        gnome) echo "myna-e2e-$1" ;;
        *) echo "myna-e2e-$E2E_DESKTOP-$1" ;;
    esac
}

check_desktop() {
    case " $DESKTOPS " in *" $E2E_DESKTOP "*) ;; *) echo "--desktop one of: $DESKTOPS" >&2; exit 2 ;; esac
}

# The cloud-init user-data for the desktop: the shared part merged with the
# desktop's, as a MIME multipart whose parts append their lists.
user_data() {
    local b=e2e-boundary f
    printf 'Content-Type: multipart/mixed; boundary="%s"\nMIME-Version: 1.0\n\n' "$b"
    for f in common "$E2E_DESKTOP"; do
        printf -- '--%s\nContent-Type: text/cloud-config\nX-Merge-Type: list(append)+dict(recurse_array)+str()\n\n' "$b"
        cat "$E2E_ROOT/vm/user-data.$f"
        printf '\n'
    done
    printf -- '--%s--\n' "$b"
}

check_release() {
    case " $RELEASES " in *" $1 "*) ;; *) echo "--release one of: $RELEASES" >&2; exit 2 ;; esac
}

# The released series boot from cloud-images' releases stream, the devel
# series from the daily one.
image() {
    case $1 in stonking) echo "ubuntu-daily:$1" ;; *) echo "ubuntu:$1" ;; esac
}

# Run a command as the autologin user, inside its graphical session's
# environment. An X11 desktop's display server is reachable with DISPLAY and
# the session's cookie; GNOME's Wayland session needs neither.
on_vm() {
    local vm=$1; shift
    local x11=()
    [ "$E2E_DESKTOP" != xubuntu ] || x11=(--env DISPLAY=:0 --env XAUTHORITY=/home/ubuntu/.Xauthority)
    lxc exec "$vm" --user 1000 --group 1000 --cwd /home/ubuntu \
        --env HOME=/home/ubuntu --env USER=ubuntu \
        --env XDG_RUNTIME_DIR=/run/user/1000 \
        --env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
        "${x11[@]}" -- bash -c "$*"
}

# Wait for cloud-init to finish. Short polls, not one `status --wait`: the
# agent restarts while cloud-init upgrades packages and takes a long exec
# down with it. A copy has cloud-init disabled.
wait_booted() {
    local status
    for _ in $(seq 600); do
        # Exit 2 is "done" with recoverable errors: keep the output.
        status=$(lxc exec "$1" -- cloud-init status 2>/dev/null || true)
        case $status in
            *done*|*disabled*) return 0 ;;
            *error*) lxc exec "$1" -- cloud-init status --long >&2; return 1 ;;
        esac
        sleep 2
    done
    echo "$1: cloud-init not done after 20 min" >&2
    return 1
}

# Probe run in the VM as the user: succeeds once the desktop's session is
# settled. The bus socket existing is not the session being settled: an app
# launched before the shell owns its name can exit early.
session_up() {
    case $E2E_DESKTOP in
        gnome) echo 'gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.freedesktop.DBus.Peer.Ping' ;;
        # The session manager, the settings daemon (live keyboard shortcuts
        # need it) and a usable X display.
        xubuntu) echo 'pgrep -u 1000 -x xfce4-session && pgrep -u 1000 -x xfsettingsd && xdpyinfo' ;;
    esac
}

# Boot, then wait for the desktop's session.
wait_ready() {
    local vm=$1
    wait_booted "$vm"
    for _ in $(seq 90); do
        on_vm "$vm" "$(session_up)" >/dev/null 2>&1 && { sleep 10; return 0; }
        sleep 5
    done
    echo "$vm: session never settled" >&2
    return 1
}

# Wait until snapd has seeded and has no change in flight (a failed change
# is over, and stays in the list: a retry after a download error must pass).
wait_snapd_idle() {
    # shellcheck disable=SC2016 # expands in the VM
    lxc exec "$1" -- bash -c 'snap wait system seed.loaded
for _ in $(seq 300); do
    [ -z "$(snap changes | awk "NR>1 && \$2 != \"Done\" && \$2 != \"Hold\" && \$2 != \"Error\" && \$2 != \"Undone\"")" ] && exit 0
    sleep 5
done
echo "snapd still busy" >&2; exit 1'
}

# Shut down cleanly before a snapshot; a devel-series guest can outlast
# LXD's default timeout, and then power-off is the lesser harm.
stop_vm() { lxc stop --timeout 180 "$1" || lxc stop --force "$1"; }

snapshots() { lxc query "/1.0/instances/$1/snapshots?recursion=1" | jq -r '.[].name'; }
