#!/bin/bash
# Prime the qemu image spread boots (~/.spread/qemu/ubuntu-24.04-64.img).
# Mirrors the "Build + prime qemu image" step in .github/workflows/spread.yml.
# One-time ~1 GB download; the primed image is reused on every spread run.
#
# The image is staged under a .tmp name and only moved into place after a
# successful cloud-init + poweroff, so a failed run cannot leave a
# downloaded-but-unprimed image that the idempotency check would skip.
set -euo pipefail

IMG="$HOME/.spread/qemu/ubuntu-24.04-64.img"
DISK_SIZE="${SPREAD_DISK_SIZE:-16G}"
TMP="$IMG.tmp"
# Tools first, image second. A primed image says nothing about whether the
# emulator is installed, and spread needs qemu-system-x86_64 on PATH to launch
# the VM even when it has nothing left to build. Returning early on a primed
# image before this check is why a *cache hit* failed CI with "qemu-system-x86_64:
# executable file not found" while cache misses passed.
missing=0
for cmd in qemu-system-x86_64 cloud-localds genisoimage sshpass wget openssl snap; do
    command -v "$cmd" >/dev/null || missing=1
done
if [ "$missing" -eq 1 ]; then
    sudo apt-get update
    # genisoimage is also cloud-image-utils' own dependency for cloud-localds;
    # named explicitly because it is used directly below, not just as a
    # transitive Depends: it should stay found even if that stops being true.
    sudo apt-get install -y qemu-system-x86 cloud-image-utils genisoimage sshpass wget openssl
fi

if [ -f "$IMG" ]; then
    echo "spread image already primed: $IMG"
    exit 0
fi

mkdir -p "$(dirname "$IMG")"
cd "$(dirname "$IMG")"

if [ ! -f "$TMP" ]; then
    wget -q https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img -O "$TMP"
fi

# The cloud image ships a 3.5G disk, which the base system plus the snaps a
# task installs will fill (adapter-smoke and thread-pinning each sideload a
# real inference snap and its model component). qcow2 is sparse, so the extra
# virtual size costs nothing until it is used; cloud-init's growpart expands
# the root partition on first boot.
qemu-img resize "$TMP" "$DISK_SIZE"

HASH=$(openssl passwd -6 ubuntu)
printf '#cloud-config\nssh_pwauth: true\nusers:\n  - name: ubuntu\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    lock_passwd: false\n    passwd: %s\n' "$HASH" > user-data
printf 'instance-id: myna-spread-seed\nlocal-hostname: myna-spread\n' > meta-data
cloud-localds seed.iso user-data meta-data

# myna-snap's `hud` app uses the `gnome` extension, which makes ANY install of
# the myna snap - confined-e2e never even runs `hud` - pull gnome-46-2404,
# gtk-common-themes and mesa-2404 in as prerequisites. qemu's usermode (SLIRP)
# networking inside the guest is too slow for that download to land inside
# install_snap's 300s timeout (observed: low single-digit % per minute, so
# tens of minutes for ~100 MB), so every confined-e2e / control-socket run
# failed on "install-snap change in progress" once the extension was added
# (f5e5a63). The store itself is not the bottleneck - fetching the same snaps
# on the host below takes seconds - so download them here (fast, real
# networking) and hand the files to the guest on a second read-only ISO drive
# for an offline `snap ack` + `snap install`, entirely off the slow path.
if [ ! -f gnome-ext.iso ]; then
    rm -rf gnome-ext-snaps
    mkdir gnome-ext-snaps
    for s in gnome-46-2404 gtk-common-themes mesa-2404; do
        (cd gnome-ext-snaps && snap download "$s")
    done
    genisoimage -quiet -output gnome-ext.iso -volid GNOMEEXT -joliet -rock gnome-ext-snaps
fi

QEMU=""
cleanup() {
    if [ -n "$QEMU" ]; then
        kill "$QEMU" 2>/dev/null || true
    fi
}
trap cleanup EXIT

qemu-system-x86_64 -enable-kvm -m 2G -smp 2 -nographic \
    -drive file="$TMP",if=virtio,format=qcow2 \
    -drive file=seed.iso,if=virtio,format=raw \
    -drive file=gnome-ext.iso,if=virtio,format=raw \
    -netdev user,id=n1,hostfwd=tcp::10022-:22 \
    -device virtio-net-pci,netdev=n1 &
QEMU=$!

# UserKnownHostsFile=/dev/null alongside StrictHostKeyChecking=no: this port
# is fixed (10022) and every boot generates a fresh host key, so without it
# StrictHostKeyChecking=no's own default behavior - accept AND remember the
# key - leaves a stale entry that rejects the *next* re-prime's (different)
# key as a possible MITM attack, hanging every later ssh call here on a
# password prompt it can never see. Found by re-running this script
# repeatedly while developing the gnome-ext.iso staging above.
ready=0
for _ in $(seq 1 90); do
    if sshpass -p ubuntu ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
        -o PreferredAuthentications=password -o PubkeyAuthentication=no \
        -p 10022 ubuntu@localhost 'cloud-init status --wait' 2>/dev/null; then
        ready=1
        break
    fi
    sleep 5
done

if [ "$ready" -ne 1 ]; then
    echo "timed out waiting for cloud-init in the priming VM" >&2
    exit 1
fi

# Install the prerequisite snaps staged on gnome-ext.iso above, offline (no
# store fetch from the guest at all): ack each assertion, then sideload the
# matching .snap. Baking them into the image here means the later
# `snap install myna` in a task finds them already present.
sshpass -p ubuntu ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    -p 10022 ubuntu@localhost '
set -e
dev=$(readlink -f /dev/disk/by-label/GNOMEEXT)
mnt=$(mktemp -d)
sudo mount -o ro "$dev" "$mnt"
for f in "$mnt"/*.assert; do sudo snap ack "$f"; done
for f in "$mnt"/*.snap; do sudo snap install "$f"; done
sudo umount "$mnt"
'

sshpass -p ubuntu ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    -p 10022 ubuntu@localhost 'sudo poweroff' || true
wait "$QEMU" || true
QEMU=""
trap - EXIT

mv "$TMP" "$IMG"
rm -f seed.iso user-data meta-data
# gnome-ext.iso is NOT removed: it is the expensive part to rebuild (three
# store downloads + packing ~1 GB into an ISO), and keeping it means a later
# re-prime (e.g. after deleting $IMG for a disk-size change) skips straight to
# the cheap steps instead of repeating it. Only the loose .snap/.assert
# staging directory goes, now that they are packed.
rm -rf gnome-ext-snaps
echo "primed: $IMG"
