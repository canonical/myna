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
TMP="$IMG.tmp"
if [ -f "$IMG" ]; then
    echo "spread image already primed: $IMG"
    exit 0
fi

missing=0
for cmd in qemu-system-x86_64 cloud-localds sshpass wget openssl; do
    command -v "$cmd" >/dev/null || missing=1
done
if [ "$missing" -eq 1 ]; then
    sudo apt-get update
    sudo apt-get install -y qemu-system-x86 cloud-image-utils sshpass wget openssl
fi

mkdir -p "$(dirname "$IMG")"
cd "$(dirname "$IMG")"

if [ ! -f "$TMP" ]; then
    wget -q https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img -O "$TMP"
fi

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
