# shellcheck shell=bash
# Shared helpers for the e2e scripts. Source, do not execute.

# lxc reads stdin even when it has nothing to send; under ssh (Testflinger)
# that hangs the caller.
[ -t 0 ] || exec </dev/null

E2E_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
export RUN_DIR=$E2E_ROOT/.run
RELEASES="noble resolute stonking"

vm_name() { echo "myna-e2e-$1"; }

check_release() {
    case " $RELEASES " in *" $1 "*) ;; *) echo "--release one of: $RELEASES" >&2; exit 2 ;; esac
}

# The released series boot from cloud-images' releases stream, the devel
# series from the daily one.
image() {
    case $1 in stonking) echo "ubuntu-daily:$1" ;; *) echo "ubuntu:$1" ;; esac
}

# Run a command as the autologin user, inside its graphical session's
# environment.
on_vm() {
    local vm=$1; shift
    lxc exec "$vm" --user 1000 --group 1000 --cwd /home/ubuntu \
        --env HOME=/home/ubuntu --env USER=ubuntu \
        --env XDG_RUNTIME_DIR=/run/user/1000 \
        --env DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus \
        -- bash -c "$*"
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

# Boot, then wait for gnome-shell on the autologin session's bus. The bus
# socket existing is not the session being settled: an app launched before
# the shell owns its name can exit early.
wait_ready() {
    local vm=$1
    wait_booted "$vm"
    for _ in $(seq 90); do
        on_vm "$vm" 'gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.freedesktop.DBus.Peer.Ping' \
            >/dev/null 2>&1 && { sleep 10; return 0; }
        sleep 5
    done
    echo "$vm: session never settled" >&2
    return 1
}

# Wait until snapd has seeded and has no change in flight.
wait_snapd_idle() {
    # shellcheck disable=SC2016 # expands in the VM
    lxc exec "$1" -- bash -c 'snap wait system seed.loaded
for _ in $(seq 300); do
    [ -z "$(snap changes | awk "NR>1 && \$2 != \"Done\" && \$2 != \"Hold\"")" ] && exit 0
    sleep 5
done
echo "snapd still busy" >&2; exit 1'
}

# Shut down cleanly before a snapshot; a devel-series guest can outlast
# LXD's default timeout, and then power-off is the lesser harm.
stop_vm() { lxc stop --timeout 180 "$1" || lxc stop --force "$1"; }

snapshots() { lxc query "/1.0/instances/$1/snapshots?recursion=1" | jq -r '.[].name'; }
