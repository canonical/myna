# shellcheck shell=bash
# Shared helpers for the e2e VM scripts. Source, do not execute.

E2E_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUN_DIR=$E2E_ROOT/.run
IMAGES_DIR=/var/lib/libvirt/images
KEY=$RUN_DIR/id_ed25519

vm_name() { echo "myna-e2e-$1"; }

# Releases we provision, and their cloud-image directory names. The devel
# series lives under its codename on cloud-images while in development.
image_url() {
    case $1 in
        noble|resolute|stonking) echo "https://cloud-images.ubuntu.com/$1/current/$1-server-cloudimg-amd64.img" ;;
        *) echo "unknown release $1" >&2; return 2 ;;
    esac
}

ensure_key() {
    mkdir -p "$RUN_DIR"
    [ -f "$KEY" ] || ssh-keygen -q -t ed25519 -N "" -f "$KEY"
}

# Wait until the VM has an IPv4 address; print it. The guest agent only
# answers once cloud-init has installed it, so prefer the DHCP lease table.
vm_ip() {
    local name=$1 i mac ip
    mac=$(virsh domiflist "$name" 2>/dev/null | awk '/network/ {print $5; exit}')
    for i in $(seq 90); do
        # A rebooted VM may hold several leases; probe every candidate and
        # take the one that answers on 22 (the agent answers only once
        # cloud-init has installed it, so it cannot be the only source).
        for ip in $(virsh net-dhcp-leases default 2>/dev/null | awk -v m="$mac" '$0 ~ m {split($5, a, "/"); print a[1]}') \
                  $(virsh domifaddr "$name" --source agent 2>/dev/null | awk '/ipv4/ {split($4, a, "/"); print a[1]}'); do
            if (exec 3<>"/dev/tcp/$ip/22") 2>/dev/null; then exec 3>&- 3<&-; echo "$ip"; return 0; fi
        done
        sleep 2
    done
    echo "no IP for $name after 3 min" >&2
    return 1
}

vm_ssh() {
    local ip=$1; shift
    ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" \
        -o ConnectTimeout=5 "ubuntu@$ip" "$@"
}

# Wait for SSH, then for cloud-init to be done, then for the autologin
# session's bus.
wait_ready() {
    local ip=$1 i
    for i in $(seq 120); do
        vm_ssh "$ip" true 2>/dev/null && break
        sleep 5
    done
    vm_ssh "$ip" 'cloud-init status --wait >/dev/null 2>&1 || true'
    # The bus socket existing is not the session being settled: right after
    # autologin, shell and portals are still coming up, and an app launched
    # in that window can exit early. Wait for gnome-shell's name, then let
    # the session breathe.
    for i in $(seq 90); do
        if vm_ssh "$ip" 'test -S /run/user/1000/bus && gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.freedesktop.DBus.Peer.Ping' >/dev/null 2>&1; then
            sleep 10
            return 0
        fi
        sleep 5
    done
    echo "session never settled on $ip" >&2
    return 1
}

# Wait until snapd has finished seeding and has nothing in progress.
wait_snapd_idle() {
    local ip=$1
    vm_ssh "$ip" 'sudo snap wait system seed.loaded 2>/dev/null || true
for _ in $(seq 300); do
    [ -z "$(snap changes 2>/dev/null | awk "NR>1 && \\$NF != \"Done\" && \\$NF != \"Hold\"")" ] && exit 0
    sleep 5
done
echo "snapd still busy" >&2; exit 1'
}

shutdown_vm() {
    local name=$1 i
    virsh shutdown "$name" >/dev/null 2>&1 || true
    for i in $(seq 60); do
        [ "$(virsh domstate "$name" 2>/dev/null)" = "shut off" ] && return 0
        sleep 3
    done
    echo "$name did not shut down" >&2
    return 1
}

start_vm() {
    local name=$1
    [ "$(virsh domstate "$name")" = "running" ] || virsh start "$name" >/dev/null
    vm_ip "$name"
}
