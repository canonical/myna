#!/usr/bin/env bash
# Provision a GNOME desktop VM for the Myna Settings e2e suites, from the
# release's Ubuntu cloud image. Ends with the VM shut off and three disk
# snapshots that mirror the states the suites declare:
#
#   bare             desktop up, no myna snaps, no user-daemons flag
#   components-only  flag on, myna + myna-parakeet installed from latest/edge,
#                    backend not connected, no shortcut
#   installed        components-only plus myna:backend connected, the daemon
#                    restarted, and (where Activation is "control", i.e.
#                    noble) the Super+J custom shortcut onboarding writes
#
# Suites revert to these with run-suite.sh; reprovision on a new edge
# revision or a stale image with --force.
#
# Usage: provision.sh --release noble|resolute|stonking [--force] [--keep-running]
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=lib.sh
source "$HERE/lib.sh"

RELEASE= FORCE= KEEP=
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --force) FORCE=1; shift ;;
        --keep-running) KEEP=1; shift ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
case $RELEASE in noble|resolute|stonking) ;; *) echo "--release noble|resolute|stonking" >&2; exit 2 ;; esac

VM=$(vm_name "$RELEASE")
IMAGE=$IMAGES_DIR/$VM.qcow2
SEED=$IMAGES_DIR/$VM-seed.iso
URL=$(image_url "$RELEASE")
DISK_SIZE=${DISK_SIZE:-16G}

for cmd in virsh qemu-img cloud-localds ssh-keygen; do
    command -v "$cmd" >/dev/null || { echo "missing $cmd (apt: qemu-system-x86 libvirt-daemon-system cloud-image-utils)" >&2; exit 1; }
done
ensure_key

if [ -n "$FORCE" ]; then
    virsh destroy "$VM" >/dev/null 2>&1 || true
    virsh undefine "$VM" --remove-all-storage >/dev/null 2>&1 || true
    sudo rm -f "$IMAGE" "$SEED"
fi

if ! virsh dominfo "$VM" >/dev/null 2>&1; then
    echo "== building image for $RELEASE from $URL"
    mkdir -p "$RUN_DIR/cache"
    BASE=$RUN_DIR/cache/$(basename "$URL")
    if [ ! -f "$BASE" ]; then
        wget -q --show-progress "$URL" -O "$BASE.tmp"
        mv "$BASE.tmp" "$BASE"
    fi
    # qcow2 is sparse; virtual size costs nothing until written.
    qemu-img resize "$BASE" "$DISK_SIZE"
    sudo qemu-img convert -O qcow2 "$BASE" "$IMAGE"
    sudo chown libvirt-qemu:kvm "$IMAGE"

    export SSH_PUBLIC_KEY HOSTNAME=$VM
    SSH_PUBLIC_KEY=$(cat "$KEY.pub")
    envsubst '${SSH_PUBLIC_KEY} ${RELEASE} ${HOSTNAME}' < "$HERE/user-data" > "$RUN_DIR/user-data.$RELEASE"
    envsubst '${SSH_PUBLIC_KEY} ${RELEASE} ${HOSTNAME}' < "$HERE/meta-data" > "$RUN_DIR/meta-data.$RELEASE"
    cloud-localds "$RUN_DIR/seed-$RELEASE.iso" "$RUN_DIR/user-data.$RELEASE" "$RUN_DIR/meta-data.$RELEASE"
    sudo mv "$RUN_DIR/seed-$RELEASE.iso" "$SEED"
    sudo chown libvirt-qemu:kvm "$SEED"

    env VM_NAME=$VM IMAGE_FILE=$IMAGE SEED_FILE=$SEED \
        envsubst '${VM_NAME} ${IMAGE_FILE} ${SEED_FILE}' < "$HERE/domain.xml" > "$RUN_DIR/domain-$RELEASE.xml"
    virsh define "$RUN_DIR/domain-$RELEASE.xml" >/dev/null
    echo "== first boot: cloud-init installs the desktop, then powers off"
    virsh start "$VM" >/dev/null
    IP=$(vm_ip "$VM")
    # cloud-init's power_state shuts the VM down when done.
    wait_ready "$IP"
    shutdown_vm "$VM"
fi

# One stage: boot, run the stage's commands over SSH, shut down, snapshot.
stage() {
    local name=$1; shift
    echo "== stage $name"
    local ip
    ip=$(start_vm "$VM")
    wait_ready "$ip"
    wait_snapd_idle "$ip"
    vm_ssh "$ip" "$*"
    shutdown_vm "$VM"
    virsh snapshot-create-as "$VM" "$name" >/dev/null
    virsh snapshot-list "$VM" | tail -2
}

SNAPS=$(virsh snapshot-list "$VM" --name 2>/dev/null)
if ! grep -qx bare <<< "$SNAPS"; then
    stage bare 'true'
fi

if ! grep -qx components-only <<< "$SNAPS"; then
    stage components-only '
        sudo snap set system experimental.user-daemons=true
        sudo snap install --edge myna
        sudo snap install --edge myna-parakeet
        for _ in $(seq 300); do
            [ -z "$(snap changes | awk "NR>1 && \\$NF != \"Done\" && \\$NF != \"Hold\"")" ] && exit 0
            sleep 5
        done; exit 1'
fi

if ! grep -qx installed <<< "$SNAPS"; then
    stage installed '
        set -e
        if ! snap connections myna | awk "\$2 == \"myna:backend\" && \$3 != \"-\"" | grep -q .; then
            sudo snap connect myna:backend myna-parakeet:provider
        fi
        export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
        systemctl --user restart snap.myna.myna.service
        # The daemon decides its activation mode only once the portal runs,
        # and in a fresh session nothing has started it yet. Introspecting
        # dbus-activates it.
        gdbus introspect --session -d org.freedesktop.portal.Desktop \
            -o /org/freedesktop/portal/desktop >/dev/null 2>&1 || true
        activation=
        for _ in $(seq 60); do
            activation=$(gdbus call --session -d com.canonical.Myna.Dictation -o /com/canonical/Myna/Dictation \
                -m org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation Activation 2>/dev/null || true)
            [ -n "$activation" ] && [ "$activation" != "(<'\'\'>,)" ] && break
            sleep 3
        done
        systemctl --user is-active snap.myna.myna.service
        if [[ $activation == *control* ]]; then
            # What Myna Settings writes on noble (adapters/desktop_shortcut.rs).
            list=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings
            entry=$list/myna/
            current=$(dconf read $list)
            case $current in
                *"$entry"*) ;;
                ""|"@as []"|"[]") dconf write $list "['"'"'$entry'"'"']" ;;
                *) dconf write $list "$(printf "%s" "$current" | sed "s|^\[|['"'"'$entry'"'"', |")" ;;
            esac
            dconf write ${entry}name "'"'"'Dictation'"'"'"
            dconf write ${entry}command "'"'"'/snap/bin/myna.toggle'"'"'"
            dconf write ${entry}binding "'"'"'<Super>j'"'"'"
        fi'
fi

if [ -z "$KEEP" ]; then
    shutdown_vm "$VM" || true
fi
echo "== $VM provisioned; snapshots:"
virsh snapshot-list "$VM"
