#!/bin/bash
# Drive the working tree's Myna Settings in a provisioned e2e VM and take
# screenshots. Builds myna-config once in the myna-noble workshop (GTK
# 4.14/adw 1.5 floor; the binary runs on every series), ships it to the VM,
# runs it under its own Xvfb (x11, 1100x800) on the VM user's real session
# bus, drives it through AT-SPI (shot-driver.py) and copies the PNGs back.
#
# Ported from ~/myna-onboarding-stable/tools/shot.sh: hosts are libvirt VMs
# provisioned by vm/provision.sh instead of physical machines, so the
# live-desktop guards (stonking proxy, --real refusals) are gone: every VM
# is disposable.
#
# Usage: shot.sh --release noble|resolute|stonking [options] [step ...]
#   --no-build           reuse the last built binary (.run/build/myna-config)
#   --polkit MODE        none (default) | allow | deny | cancel, each also as
#                        slow-<mode> (answer held 6 s), long-cancel (a cancel
#                        held 45 s, past a 30 s read): a temporary polkit rule
#                        for snapd's actions (see --polkit-actions); cancel
#                        answers through cancel-agent.py, like pressing Cancel
#   --polkit-actions L   comma list of snapd action suffixes the rule covers
#                        (default manage,manage-configuration,manage-interfaces)
#   --apply-polkit MODE  none | allow | deny | cancel | slow-allow | slow-deny:
#                        a rule for the pkexec --apply-plan prompt
#   --monitor NAME       log processes, snapd changes and the journal to
#                        the artifacts dir as NAME-{monitor,journal}.log
#   --keyfile FILE       seed the dictation settings keyfile (no restore)
#   --language LIST      run the app with LANGUAGE=LIST
#   --out DIR            local output dir (default .run/shots)
#   --real               run on the VM's real Wayland session instead of Xvfb.
#                        No xdotool there: drive with activate:, shoot with
#                        realshot:, reach other apps' dialogs with other:
#   --fake-daemon        run the app and the driver on a private session bus
#                        where fake-daemon.py stands in for the dictation daemon
# Steps: see shot-driver.py. "{rel}" in a step becomes the release name.
#
# State is the suite's business: run-suite.sh reverts a snapshot first.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=../vm/lib.sh
source "$HERE/../vm/lib.sh"
REPO=${MYNA_REPO:-$(git -C "$HERE" rev-parse --show-toplevel)}
CACHE=$E2E_ROOT/.run/build
OUT=$E2E_ROOT/.run/shots
HOST=
BUILD=1
POLKIT=none
POLKIT_ACTIONS=manage,manage-configuration,manage-interfaces
APPLY_POLKIT=none
MONITOR=
KEYFILE=
LANGUAGE_LIST=
REAL=0
FAKE=0
STEPS=()

while [ $# -gt 0 ]; do
    case $1 in
        --release) HOST=$2; shift ;;
        --no-build) BUILD=0 ;;
        --polkit) POLKIT=$2; shift ;;
        --polkit-actions) POLKIT_ACTIONS=$2; shift ;;
        --apply-polkit) APPLY_POLKIT=$2; shift ;;
        --monitor) MONITOR=$2; shift ;;
        --keyfile) KEYFILE=$2; shift ;;
        --out) OUT=$2; shift ;;
        --language) LANGUAGE_LIST=$2; shift ;;
        --real) REAL=1 ;;
        --fake-daemon) FAKE=1 ;;
        -h|--help) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) STEPS+=("$1") ;;
    esac
    shift
done
case $HOST in noble|resolute|stonking) ;; *) echo "shot.sh: --release noble|resolute|stonking" >&2; exit 2 ;; esac
VM=$(vm_name "$HOST")
[ ${#STEPS[@]} -gt 0 ] || STEPS=(wait:3 "shot:{rel}.png")
STEPS=("${STEPS[@]//\{rel\}/$HOST}")
case $POLKIT in none|allow|deny|cancel|slow-allow|slow-deny|slow-cancel|long-cancel) ;; *) echo "bad --polkit $POLKIT" >&2; exit 2 ;; esac
case $APPLY_POLKIT in none|allow|deny|cancel|slow-allow|slow-deny) ;; *) echo "bad --apply-polkit $APPLY_POLKIT" >&2; exit 2 ;; esac
mkdir -p "$CACHE" "$OUT" "$E2E_ROOT/.run/logs"

if [ $BUILD = 1 ]; then
    echo "shot.sh: building myna-config in myna-noble" >&2
    # A worktree's .git file points at a host path that does not resolve
    # inside the workshop container, so the build cannot run dev/version.sh
    # there. Stage client/.version (what snap packaging does) for the
    # duration of the build.
    (cd "$REPO" && ./dev/version.sh > client/.version)
    trap 'rm -f "$REPO/client/.version"' EXIT
    (cd "$REPO" && systemd-run --user --scope -q -p MemoryHigh=infinity \
        workshop exec myna-noble -- bash -c \
        'export CARGO_TARGET_DIR=$HOME/target; cd /project/client && cargo build --release -q -p myna-config --bin myna-config' >&2)
    rm -f "$REPO/client/.version"; trap - EXIT
    (cd "$REPO" && workshop exec myna-noble -- cat /home/workshop/target/release/myna-config) > "$CACHE/myna-config.new"
    mv "$CACHE/myna-config.new" "$CACHE/myna-config"
    chmod +x "$CACHE/myna-config"
fi
[ -x "$CACHE/myna-config" ] || { echo "no binary; drop --no-build" >&2; exit 2; }

IP=$(start_vm "$VM" >/dev/null; vm_ip "$VM")
ADDR_SSH=(vm_ssh "$IP")
DIR=/home/ubuntu/myna-shot
vm_ssh "$IP" "mkdir -p $DIR/schemas $DIR/out && rm -f $DIR/out/*.png $DIR/out/*.log $DIR/seed-keyfile"
FILES=("$CACHE/myna-config" "$HERE/shot-driver.py" "$HERE/cancel-agent.py" "$HERE/snapd-rest.py" "$HERE/realshot.py" "$HERE/rdkeys.py" "$HERE/fake-daemon.py")
SCHEMAS=("$REPO"/client/data/glib-2.0/schemas/*.gschema.xml)
scp -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" -q \
    "${FILES[@]}" "ubuntu@$IP:$DIR/"
scp -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" -q \
    "${SCHEMAS[@]}" "ubuntu@$IP:$DIR/schemas/"
[ -z "$KEYFILE" ] || scp -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile="$RUN_DIR/known_hosts" -q "$KEYFILE" "ubuntu@$IP:$DIR/seed-keyfile"

ARGS=$(printf '%q ' "$HOST" "$DIR" "$POLKIT" "$POLKIT_ACTIONS" "$APPLY_POLKIT" "$MONITOR" "$LANGUAGE_LIST" "$REAL" "$FAKE" "${STEPS[@]}")
rc=0
ssh -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" \
    "ubuntu@$IP" "bash -s -- $ARGS" < "$HERE/shot-remote.sh" || rc=$?

scp -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" -q \
    "ubuntu@$IP:$DIR/out/*.log" "$E2E_ROOT/.run/logs/" 2>/dev/null || :
scp -i "$KEY" -o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile="$RUN_DIR/known_hosts" -q \
    "ubuntu@$IP:$DIR/out/*.png" "$OUT/" 2>/dev/null || :
ls -1t "$OUT"/*.png 2>/dev/null | head -n 10 >&2 || :
exit $rc
