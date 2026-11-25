#!/usr/bin/env bash
# Publish and fetch the provisioned desktop VMs as OCI artifacts on GHCR, so a
# job imports them instead of installing a desktop for 11-16 minutes.
#
# An artifact is the VM `lxc export`ed with its snapshots up to `rig` (the
# stages that need nothing from the tree under test, see provision.sh) in the
# pool's own format, plus the snapd cache provisioning saved. It lives at
# ghcr.io/<owner>/myna-e2e-<desktop>-<release>:<hash>, <hash> being the
# content of the files that decide what provisioning builds, so a change to
# them misses the image and falls back to provisioning.
#
# Usage: image.sh ref|push|pull --release noble|resolute|stonking [--desktop gnome|xubuntu|kde]
#   ref   print the artifact's reference
#   push  publish the provisioned VM (a GITHUB_TOKEN with packages: write)
#   pull  import it: exit 1 when there is none, nothing is changed then
# E2E_POOL, the pool the VM is imported into, must use the exporter's driver
# (btrfs in CI): the export is the driver's own stream.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$HERE/lib.sh"

ORAS_VERSION=1.4.0
ORAS_SHA256=d8e1dce2fd8ce996f1d901845b688eb0cf505a6082897606f053825bb47935d1
MEDIA_VM=application/vnd.myna.e2e.vm.v1.tar+zstd
MEDIA_CACHE=application/vnd.myna.e2e.snapd-cache.v1.tar

CMD=${1:-} ; shift || true
RELEASE=''
while [ $# -gt 0 ]; do
    case $1 in
        --release) RELEASE=$2; shift 2 ;;
        --desktop) E2E_DESKTOP=$2; shift 2 ;;
        -h|--help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
check_release "$RELEASE"
check_desktop
VM=$(vm_name "$RELEASE")

image_hash() {
    local f
    for f in lib.sh provision.sh user-data.common "user-data.$E2E_DESKTOP"; do
        cat "$HERE/$f"
    done | sha256sum | cut -c1-16
}

OWNER=$(printf %s "${GITHUB_REPOSITORY_OWNER:-canonical}" | tr '[:upper:]' '[:lower:]')
REF=ghcr.io/$OWNER/myna-e2e-$E2E_DESKTOP-$RELEASE:$(image_hash)

ORAS=$(command -v oras || true)
ensure_oras() {
    [ -z "$ORAS" ] || return 0
    mkdir -p "$RUN_DIR/bin"
    ORAS=$RUN_DIR/bin/oras
    [ -x "$ORAS" ] || {
        local tgz=$RUN_DIR/bin/oras.tgz
        curl -fsSL -o "$tgz" "https://github.com/oras-project/oras/releases/download/v$ORAS_VERSION/oras_${ORAS_VERSION}_linux_amd64.tar.gz"
        echo "$ORAS_SHA256  $tgz" | sha256sum -c - >/dev/null
        tar -C "$RUN_DIR/bin" -xzf "$tgz" oras
        rm "$tgz"
    }
    # GITHUB_TOKEN reads the repository's packages and, with packages: write, publishes them.
    if [ -n "${GITHUB_TOKEN:-}" ]; then
        printf %s "$GITHUB_TOKEN" | "$ORAS" login ghcr.io -u "${GITHUB_ACTOR:-github-actions}" --password-stdin >/dev/null
    fi
}

case $CMD in
    ref) echo "$REF" ;;
    push)
        ensure_oras
        snapshots "$VM" | grep -qx rig || { echo "$VM has no rig snapshot: provision it first" >&2; exit 1; }
        lxc stop --force "$VM" 2>/dev/null || true
        tmp=$(mktemp -d "$RUN_DIR/image.XXXXXX")
        trap 'rm -rf "$tmp"' EXIT
        # Only the stages the artifact promises: a later one (dictation) is the tree's.
        for s in $(snapshots "$VM" | grep -vxE 'bare|components-only|installed|localized|rig'); do
            lxc delete "$VM/$s"
        done
        lxc export "$VM" "$tmp/vm.tar.zst" --optimized-storage --compression zstd
        tar -C "$SNAPD_CACHE" -cf "$tmp/snapd-cache.tar" .
        ls -lh "$tmp"
        (cd "$tmp" && "$ORAS" push "$REF" \
            --annotation "org.opencontainers.image.source=https://github.com/${GITHUB_REPOSITORY:-canonical/myna}" \
            --annotation "org.opencontainers.image.description=Provisioned $E2E_DESKTOP $RELEASE VM for the Myna e2e suites" \
            "vm.tar.zst:$MEDIA_VM" "snapd-cache.tar:$MEDIA_CACHE")
        echo "== pushed $REF"
        ;;
    pull)
        ensure_oras
        if lxc info "$VM" >/dev/null 2>&1; then
            echo "== $VM exists: not importing $REF"
            exit 0
        fi
        tmp=$(mktemp -d "$RUN_DIR/image.XXXXXX")
        trap 'rm -rf "$tmp"' EXIT
        "$ORAS" pull "$REF" -o "$tmp" || { echo "== no image $REF" >&2; exit 1; }
        lxc import "$tmp/vm.tar.zst" ${E2E_POOL:+-s "$E2E_POOL"}
        rm "$tmp/vm.tar.zst"
        mkdir -p "$SNAPD_CACHE"
        tar -C "$SNAPD_CACHE" -xf "$tmp/snapd-cache.tar"
        snapshots "$VM" | grep -qx rig || { echo "$REF has no rig snapshot" >&2; lxc delete --force "$VM"; exit 1; }
        echo "== imported $REF"
        ;;
    *) echo "usage: image.sh ref|push|pull --release R [--desktop D]" >&2; exit 2 ;;
esac
