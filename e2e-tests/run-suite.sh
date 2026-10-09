#!/usr/bin/env bash
# Run e2e suites against a provisioned VM: per suite, boot a throwaway copy
# of the snapshot it declares (a `# snapshot: NAME` header), push the binary
# and tools, run it, pull artifacts. Local runs and CI share this entry point.
#
# Usage: run-suite.sh --release noble|resolute|stonking [--desktop gnome|xubuntu] [--binary PATH] [SUITE ...]
#   --desktop D    the desktop VM to use (default $E2E_DESKTOP, else gnome)
#   --binary PATH  test this myna-config instead of building one in the
#                  myna-noble workshop
# E2E_VM_MEMORY=4GiB sizes the guest (default 6GiB, what CI gives it).
# E2E_MYNA_SNAP=PATH installs that myna snap (`make snap-myna`) over the
# store's edge one in each run, keeping its connections (not on a machine
# that has none yet).
# Suites live in suites/; none given runs them all. The build also yields
# myna-hud-host (the Xfce HUD supervisor, which the dictation suite starts the
# way the deb's autostart entry does); it is pushed from beside --binary.
# Screenshots a suite takes under shots/<suite>/<NN>-<checkpoint>.png are
# collected in artifacts/screenshots/<suite>/ (artifacts/<tag>-<suite>/out/
# keeps everything else).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=e2e-tests/vm/lib.sh
source "$HERE/vm/lib.sh"

REL='' BINARY='' SUITES=()
while [ $# -gt 0 ]; do
    case $1 in
        --release) REL=$2; shift 2 ;;
        --desktop) E2E_DESKTOP=$2; shift 2 ;;
        --binary) BINARY=$2; shift 2 ;;
        -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) SUITES+=("$1"); shift ;;
    esac
done
check_release "$REL"
check_desktop
VM=$(vm_name "$REL")
if [ ${#SUITES[@]} = 0 ]; then
    for f in "$HERE"/suites/*.sh; do
        [ "${f##*/}" = lib.sh ] || SUITES+=("$(basename "$f" .sh)")
    done
fi
# Artifacts of the GNOME runs keep their release-only names.
TAG=$REL; [ "$E2E_DESKTOP" = gnome ] || TAG=$E2E_DESKTOP-$REL
REPO=$(git -C "$HERE" rev-parse --show-toplevel)

if [ -z "$BINARY" ]; then
    # The binary is built once on the GTK 4.14/adw 1.5 floor and runs on every
    # series. A worktree's .git does not resolve inside the workshop, so the
    # version is staged the way snap packaging does it.
    echo "== building myna-config in myna-noble"
    "$REPO/dev/version.sh" > "$REPO/client/.version"
    trap 'rm -f "$REPO/client/.version"' EXIT
    # shellcheck disable=SC2016 # expands in the workshop
    (cd "$REPO" && workshop exec myna-noble -- bash -c \
        'export CARGO_TARGET_DIR=$HOME/target; cd /project/client && cargo build --release -q -p myna-config --bin myna-config --bin myna-hud-host')
    BINARY=$RUN_DIR/myna-config
    mkdir -p "$RUN_DIR"
    for bin in myna-config myna-hud-host; do
        (cd "$REPO" && workshop exec myna-noble -- cat /home/workshop/target/release/$bin) > "$RUN_DIR/$bin"
    done
fi

rc=0
for SUITE in "${SUITES[@]}"; do
    FILE=$HERE/suites/$SUITE.sh
    SNAP=$(sed -n 's/^# snapshot: //p' "$FILE")
    snapshots "$VM" | grep -qx "$SNAP" \
        || { echo "$VM lacks snapshot $SNAP; run vm/provision.sh --release $REL --desktop $E2E_DESKTOP" >&2; exit 1; }
    ARTIFACTS=$RUN_DIR/artifacts/$TAG-$SUITE
    rm -rf "$ARTIFACTS" "$RUN_DIR/artifacts/screenshots/$SUITE"; mkdir -p "$ARTIFACTS"

    echo "== $SUITE: from $VM/$SNAP"
    RUN=$VM-run
    lxc delete --force "$RUN" 2>/dev/null || true
    lxc copy "$VM/$SNAP" "$RUN" -c limits.memory="$E2E_VM_MEMORY"
    lxc start "$RUN"
    wait_ready "$RUN"
    # Not over a machine without myna (`bare`): onboarding installs the store's.
    if [ -n "${E2E_MYNA_SNAP:-}" ] && lxc exec "$RUN" -- snap list myna >/dev/null 2>&1; then
        lxc file push "$E2E_MYNA_SNAP" "$RUN/root/myna.snap"
        lxc exec "$RUN" -- snap install --dangerous /root/myna.snap
    fi

    on_vm "$RUN" 'mkdir -p myna-shot/schemas'
    HUD_HOST=$(dirname "$BINARY")/myna-hud-host
    [ -e "$HUD_HOST" ] || HUD_HOST=''
    lxc file push --uid 1000 --gid 1000 --mode 0755 "$BINARY" ${HUD_HOST:+"$HUD_HOST"} "$HERE"/tools/* "$RUN/home/ubuntu/myna-shot/"
    lxc file push --uid 1000 --gid 1000 "$REPO"/client/data/glib-2.0/schemas/*.gschema.xml "$RUN/home/ubuntu/myna-shot/schemas/"

    src=0
    VM=$RUN REL=$REL DESKTOP=$E2E_DESKTOP bash "$FILE" || src=$?
    echo "== $SUITE rc=$src"
    [ $src = 0 ] || rc=1

    lxc file pull -r "$RUN/home/ubuntu/myna-shot/out" "$ARTIFACTS/" || true
    if [ -d "$ARTIFACTS/out/shots/$SUITE" ]; then
        mkdir -p "$RUN_DIR/artifacts/screenshots"
        mv "$ARTIFACTS/out/shots/$SUITE" "$RUN_DIR/artifacts/screenshots/$SUITE"
    fi
    lxc exec "$RUN" -- journalctl -b --no-pager -o short-precise > "$ARTIFACTS/journal.log" || true
    lxc delete --force "$RUN"
done
echo "== artifacts in $RUN_DIR/artifacts"
exit $rc
