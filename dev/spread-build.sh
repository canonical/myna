#!/bin/bash
# Build the spread runner at the commit pinned in .github/workflows/spread.yml.
#
# The snap-installed `spread` has no kvm plug and cannot drive the qemu backend
# under KVM (spread-decision.md), so local runs use a self-built binary exactly
# like the CI workflow. Idempotent: rebuilds only when the pin changes.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMMIT="$(grep -oP 'SPREAD_COMMIT: \K[0-9a-f]+' "$REPO_ROOT/.github/workflows/spread.yml")"
[ -n "$COMMIT" ] || { echo "no SPREAD_COMMIT pin in .github/workflows/spread.yml"; exit 1; }

BIN="$REPO_ROOT/.cache/spread/spread"
SRC="$REPO_ROOT/.cache/spread/src"

mkdir -p "$(dirname "$BIN")"
if [ ! -d "$SRC/.git" ]; then
    git clone https://github.com/snapcore/spread.git "$SRC"
fi
git -C "$SRC" fetch origin
# GitHub rejects shallow fetches of an unadvertised SHA; fetch full refs
# (as CI's plain `git clone` + checkout does) before checking the pin out.
git -C "$SRC" fetch --tags origin "$COMMIT" 2>/dev/null || git -C "$SRC" fetch origin
git -C "$SRC" checkout -f "$COMMIT"

# Local patch: boot guests with the host CPU model. Upstream runs qemu with
# the implicit default (qemu64), which lacks x86-64-v2; numpy 2.x wheels (and
# CTranslate2) refuse to start there, so adapter-smoke's inference step can
# never pass. `-cpu host` requires KVM, which this suite already requires
# (spread.yaml header, CI's KVM-available step). Not sent upstream as of
# 2026-08-19; the guard makes re-application idempotent.
if ! grep -q '"-cpu", "host"' "$SRC/spread/qemu.go"; then
    sed -i 's|"-enable-kvm",|"-enable-kvm",\n\t\t"-cpu", "host",|' "$SRC/spread/qemu.go"
fi

MARK="$COMMIT+cpu-host"
if [ -x "$BIN" ] && [ -f "$REPO_ROOT/.cache/spread/commit" ] \
    && [ "$(cat "$REPO_ROOT/.cache/spread/commit")" = "$MARK" ]; then
    echo "spread already built at $MARK"
    exit 0
fi
(cd "$SRC" && go build -o "$BIN" ./cmd/spread)
echo "$MARK" > "$REPO_ROOT/.cache/spread/commit"
echo "built spread at $MARK -> $BIN"
