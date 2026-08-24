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
for cmd in qemu-system-x86_64 cloud-localds sshpass wget openssl; do
    command -v "$cmd" >/dev/null || missing=1
done
if [ "$missing" -eq 1 ]; then
    sudo apt-get update
    sudo apt-get install -y qemu-system-x86 cloud-image-utils sshpass wget openssl
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
    -netdev user,id=n1,hostfwd=tcp::10022-:22 \
    -device virtio-net-pci,netdev=n1 &
QEMU=$!

ready=0
for _ in $(seq 1 90); do
    if sshpass -p ubuntu ssh -o StrictHostKeyChecking=no \
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

sshpass -p ubuntu ssh -o StrictHostKeyChecking=no \
    -o PreferredAuthentications=password -o PubkeyAuthentication=no \
    -p 10022 ubuntu@localhost 'sudo poweroff' || true
wait "$QEMU" || true
QEMU=""
trap - EXIT

mv "$TMP" "$IMG"
rm -f seed.iso user-data meta-data
echo "primed: $IMG"
