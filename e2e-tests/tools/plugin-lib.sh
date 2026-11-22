# shellcheck shell=bash
# VM-side helpers for the plugins suite, on top of dictation-lib.sh. Source
# inside the VM as the autologin user: `. myna-shot/plugin-lib.sh`.

# shellcheck source=e2e-tests/tools/dictation-lib.sh
. "$HOME/myna-shot/dictation-lib.sh"

BLOBS=$HOME/myna-shot/blobs

# What the daemon says it resolved: bound models, missing requests, steps.
daemon_status() { snap run myna --status 2>&1; }
status_has() { daemon_status | grep -q -- "$1"; }
daemon_pid() { systemctl --user show -P MainPID snap.myna.myna.service; }

# component_installed SNAP COMPONENT
component_installed() {
    snap components "$1" 2>/dev/null | awk -v c="$1+$2" '$1 == c && $2 == "installed" { found = 1 } END { exit !found }'
}

# What the CLI sideload does: one authorization, no store. The stand-in for
# Settings' Install while snapd refuses components of a sideloaded snap.
sideload() { sudo snap install --dangerous "$@"; }

# The first install of a text snap, as the set-up path leaves it: the snap
# present and its provider connected, no model component yet. A sideload gets
# none of the store's plug grants: hardware-observe (myna-text declares it for
# onnxruntime's PCI scan) is connected here, or its denial fails the run.
seed_text_snap() { # SNAP FILE
    sideload "$BLOBS/$2" && sudo snap connect myna:backend "$1:provider" || return 1
    if snap connections "$1" | grep -q "$1:hardware-observe"; then
        sudo snap connect "$1:hardware-observe"
    fi
}

# polkit authorizations the e2e rule (tools/shot.sh) granted since EPOCH.
polkit_grants() {
    sudo journalctl -t myna-e2e-polkit --no-pager --since "@$1" -o cat 2>/dev/null | grep -c '^io.snapcraft.snapd.'
}

# A text provider Myna does not curate, as a sideloaded snap.
rogue_install() {
    local dir=$HOME/myna-shot/rogue
    mkdir -p "$dir/meta" "$dir/bin"
    cat > "$dir/meta/snap.yaml" <<'YAML'
name: myna-rogue-text
version: "1"
summary: A text provider Myna does not curate (e2e fixture)
description: Serves myna-text-v1 with a cleanup model that shouts its output.
base: core24
confinement: strict
grade: devel
apps:
  server:
    command: bin/server.py
    daemon: simple
    plugs: [network-bind]
slots:
  provider:
    interface: content
    content: inference-provider
    source:
      write: [$SNAP_COMMON/share/provider]
YAML
    install -m 0755 "$HOME/myna-shot/rogue-text.py" "$dir/bin/server.py"
    snap pack "$dir" "$dir" >/dev/null && sideload "$dir"/myna-rogue-text_1_all.snap \
        && sudo snap connect myna:backend myna-rogue-text:provider
}
