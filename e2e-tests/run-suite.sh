#!/usr/bin/env bash
# Run e2e suites against a provisioned VM: per suite, boot a throwaway copy
# of the snapshot it declares (a `# snapshot: NAME` header), push the binary
# and tools, run it, pull artifacts. Local runs and CI share this entry point.
#
# Usage: run-suite.sh --release noble|resolute|stonking [--desktop gnome|xubuntu|kde] [--binary PATH] [SUITE ...]
#   --desktop D    the desktop VM to use (default $E2E_DESKTOP, else gnome)
#   --binary PATH  test this myna-config instead of building one in the
#                  myna-noble workshop
# E2E_VM_MEMORY=4GiB sizes the guest (default 6GiB, what CI gives it);
# E2E_VM_CPUS its vCPUs (default 3).
# E2E_JOBS=N runs up to N suites at once, each in its own copy (default 1,
# what CI runs; each copy takes E2E_VM_MEMORY). A parallel suite's output
# goes to its artifacts' run.log, and its lines reach the terminal prefixed.
# E2E_MYNA_SNAP=PATH installs that myna snap (`make snap-myna`) over the
# store's edge one in each run, keeping its connections (not on a machine
# that has none yet, nor for a suite headed `# myna: store`).
# E2E_FAKE_SNAP=PATH does the same for the fake backend (`make snap-fake`)
# wherever the snapshot has one.
# A suite headed `# requires: VAR` is skipped unless VAR is set.
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
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) SUITES+=("$1"); shift ;;
    esac
done
check_release "$REL"
check_desktop
VM=$(vm_name "$REL")
# A suite is a file in suites/ that is not shared code (`lib.sh`,
# `chains-lib.sh`). TAGS narrows the run to the suites carrying one of them:
# `TAGS=processing` is the Processing page, `TAGS=dictation` what reaches the
# field. SUITES names files and wins when both are given.
wanted() { # FILE
    local tags
    tags=$(sed -n 's/^# tags: //p' "$1")
    [ -n "${TAGS:-}" ] || return 0
    local tag
    for tag in ${TAGS//,/ }; do
        case ", $tags," in *" $tag,"*) return 0 ;; esac
    done
    return 1
}
if [ ${#SUITES[@]} = 0 ]; then
    for f in "$HERE"/suites/*.sh; do
        case ${f##*/} in *-lib.sh|lib.sh) continue ;; esac
        wanted "$f" && SUITES+=("$(basename "$f" .sh)")
    done
    [ ${#SUITES[@]} -gt 0 ] || { echo "no suite carries TAGS=$TAGS" >&2; exit 2; }
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

# One run of $SUITE from $SNAP into $ARTIFACTS; sets src.
attempt() {
    echo "== $SUITE: from $VM/$SNAP"
    # One copy per suite, so suites can run side by side.
    RUN=$VM-run-$SUITE
    lxc delete --force "$RUN" 2>/dev/null || true
    lxc copy "$VM/$SNAP" "$RUN" -c limits.cpu="$E2E_VM_CPUS" -c limits.memory="$E2E_VM_MEMORY"
    lxc start "$RUN"
    wait_ready "$RUN"
    # A copy boots into snapd's refresh window: a store refresh of myna in
    # flight refuses a sideload ("auto-refresh change in progress"), and one
    # after it would replace the snap under test.
    lxc exec "$RUN" -- snap refresh --hold >/dev/null
    wait_snapd_idle "$RUN"
    [ "$SNAP" != bare ] || snapd_cache_seed "$RUN"
    # Not over a machine without myna (`bare`): onboarding installs the store's.
    if [ -n "${E2E_MYNA_SNAP:-}" ] && ! grep -qx '# myna: store' "$FILE" \
        && lxc exec "$RUN" -- snap list myna >/dev/null 2>&1; then
        lxc file push "$E2E_MYNA_SNAP" "$RUN/root/myna.snap"
        # A running HUD blocks the install (issue #38): stop the daemon, whose
        # bus name keeps the shell extension or the Xfce host respawning it,
        # and wait for the HUD to go. The service comes back after.
        # shellcheck disable=SC2016 # expands in the VM
        on_vm "$RUN" 'systemctl --user stop snap.myna.myna.service
    pkill -x myna-hud-host || true
    for _ in $(seq 40); do pgrep -x myna-hud >/dev/null || exit 0; pkill -x myna-hud; sleep 0.5; done
    echo "the HUD did not stop" >&2; exit 1'
        lxc exec "$RUN" -- snap install --dangerous /root/myna.snap
        on_vm "$RUN" 'systemctl --user start snap.myna.myna.service'
    fi
    # The fake backend is this tree's too, when given: a suite's script (a
    # transcript in segments) is only as new as the snap that plays it. Over
    # the provisioned one, so its connection stays.
    if [ -n "${E2E_FAKE_SNAP:-}" ] && lxc exec "$RUN" -- snap list myna-fake-backend >/dev/null 2>&1; then
        lxc file push "$E2E_FAKE_SNAP" "$RUN/root/myna-fake-backend.snap"
        lxc exec "$RUN" -- snap install --dangerous /root/myna-fake-backend.snap
    fi

    on_vm "$RUN" 'mkdir -p myna-shot/schemas'
    HUD_HOST=$(dirname "$BINARY")/myna-hud-host
    [ -e "$HUD_HOST" ] || HUD_HOST=''
    lxc file push --uid 1000 --gid 1000 --mode 0755 "$BINARY" ${HUD_HOST:+"$HUD_HOST"} "$HERE"/tools/* "$RUN/home/ubuntu/myna-shot/"
    lxc file push --uid 1000 --gid 1000 "$REPO"/client/data/glib-2.0/schemas/*.gschema.xml "$RUN/home/ubuntu/myna-shot/schemas/"
    # The catalog snapshots the deb installs (myna-config-deb/debian/rules),
    # which Settings lists text steps from before a text snap is installed.
    # Root-owned as the deb leaves them: `push -p` makes its directories 0750
    # and owned by the caller's uid, which only chance makes the VM's user.
    lxc exec "$RUN" -- install -d -m 0755 /usr/share/myna-config/catalog
    lxc file push --uid 0 --gid 0 --mode 0644 "$REPO"/text-snap/catalog.json "$RUN/usr/share/myna-config/catalog/myna-text.json"
    lxc file push --uid 0 --gid 0 --mode 0644 "$REPO"/translation-snap/catalog.json "$RUN/usr/share/myna-config/catalog/myna-translation.json"

    # The guest's CPU time against the host's, to lay lost capture against.
    on_vm "$RUN" 'mkdir -p myna-shot/out'
    lxc exec "$RUN" -- setsid -f /home/ubuntu/myna-shot/cpu-sample.sh /home/ubuntu/myna-shot/out/cpu-guest.log
    "$HERE"/tools/cpu-sample.sh "$ARTIFACTS/cpu-host.log" & sampler=$!

    src=0
    started=$(date +%s)
    # Stamped, so `tools/breakdown.py` says where the suite's time went; the
    # suite itself reports its steps, which is all the bookkeeping it needs.
    VM=$RUN REL=$REL DESKTOP=$E2E_DESKTOP bash "$FILE" 2>&1 \
        | "$HERE"/tools/stamp.sh | tee "$ARTIFACTS/suite.log" || src=$?
    took=$(( $(date +%s) - started ))
    echo "== $SUITE rc=$src in ${took}s"
    kill "$sampler"

    lxc file pull -r "$RUN/home/ubuntu/myna-shot/out" "$ARTIFACTS/" || true
    if [ -d "$ARTIFACTS/out/shots/$SUITE" ]; then
        mkdir -p "$RUN_DIR/artifacts/screenshots"
        # Another desktop's run of the suite on this host may have put its
        # own there since this one started; mv would nest into it and fail.
        rm -rf "$RUN_DIR/artifacts/screenshots/$SUITE"
        mv "$ARTIFACTS/out/shots/$SUITE" "$RUN_DIR/artifacts/screenshots/$SUITE"
    fi
    lxc exec "$RUN" -- journalctl -b --no-pager -o short-precise > "$ARTIFACTS/journal.log" || true
    lxc delete --force "$RUN"
    # The copy booted for this suite, so its whole boot is the suite's window.
    if ! python3 -I "$REPO/tests/lib/apparmor_denials.py" "$ARTIFACTS/journal.log"; then
        echo "== $SUITE: AppArmor denials"
        src=1
    fi
    # A GTK or libadwaita critical is a bug in Settings even when every
    # widget it asserts ends up right.
    if grep -- '-CRITICAL \*\*' "$ARTIFACTS/out/app.log" 2>/dev/null; then
        echo "== $SUITE: Settings logged a critical"
        src=1
    fi
}

# One suite, start to end; its status is the subshell's.
run_one() {
    FILE=$HERE/suites/$SUITE.sh
    SNAP=$(sed -n 's/^# snapshot: //p' "$FILE")
    ARTIFACTS=$RUN_DIR/artifacts/$TAG-$SUITE
    rm -rf "$ARTIFACTS" "$RUN_DIR/artifacts/screenshots/$SUITE"; mkdir -p "$ARTIFACTS"
    attempt
    # A virtual runner's host stops the vCPUs for tens of milliseconds now
    # and then, longer than a graph cycle, and capture loses audio and ends
    # the dictation. That says nothing about the suite, so a failed suite
    # whose daemon lost audio runs once more. No loss is lab hardware's to
    # test (Testflinger), not a VM's.
    if [ "$src" != 0 ] && grep -q '\] ctrl: capture lost' "$ARTIFACTS/journal.log"; then
        echo "== $SUITE: capture lost audio, inconclusive; once more"
        rm -rf "$ARTIFACTS-lost-audio" "$RUN_DIR/artifacts/screenshots/$SUITE"
        mv "$ARTIFACTS" "$ARTIFACTS-lost-audio"; mkdir -p "$ARTIFACTS"
        attempt
    fi
    if [ -s "$ARTIFACTS/suite.log" ]; then
        python3 -I "$HERE"/tools/breakdown.py "$ARTIFACTS/suite.log" || true
    fi
    return "$src"
}

# Every suite is checked before any VM starts.
RUNNABLE=()
for SUITE in "${SUITES[@]}"; do
    FILE=$HERE/suites/$SUITE.sh
    SNAP=$(sed -n 's/^# snapshot: //p' "$FILE")
    NEEDS=$(sed -n 's/^# requires: //p' "$FILE")
    if [ -n "$NEEDS" ] && [ -z "${!NEEDS:-}" ]; then
        echo "== $SUITE: skipped, $NEEDS is not set"
        continue
    fi
    snapshots "$VM" | grep -qx "$SNAP" \
        || { echo "$VM lacks snapshot $SNAP; run vm/provision.sh --release $REL --desktop $E2E_DESKTOP" >&2; exit 1; }
    # A suite that is not out to test the store's snap needs this tree's: the
    # store's edge revision does not carry the settings a newer tree writes
    # (`snap run myna.config set chains` fails with "No such key"), and running
    # it anyway tests the wrong daemon silently.
    if [ -z "${E2E_MYNA_SNAP:-}" ] && [ "$SNAP" != bare ] && ! grep -qx '# myna: store' "$FILE"; then
        echo "== $SUITE: needs E2E_MYNA_SNAP (the snap \`make snap-myna\` built); refusing to test the store's edge one" >&2
        exit 2
    fi
    RUNNABLE+=("$SUITE")
done

# Each suite runs as a job, which keeps `set -e` in force inside it (a
# function called from `||` would run with it off). One job at a time writes
# straight to the terminal; side by side, each keeps its own log.
JOBS=${E2E_JOBS:-1}
rc=0 FAILED=()
declare -A PIDS=()
reap() {
    local pid
    wait -n -p pid "${!PIDS[@]}" || { rc=1; FAILED+=("${PIDS[$pid]}"); }
    unset "PIDS[$pid]"
}
mkdir -p "$RUN_DIR/artifacts"
for SUITE in "${RUNNABLE[@]}"; do
    while [ ${#PIDS[@]} -ge "$JOBS" ]; do reap; done
    if [ "$JOBS" -le 1 ]; then
        run_one &
    else
        run_one > >(tee "$RUN_DIR/artifacts/$TAG-$SUITE.run.log" | sed -u "s/^/[$SUITE] /") 2>&1 &
    fi
    PIDS[$!]=$SUITE
done
while [ ${#PIDS[@]} -gt 0 ]; do reap; done
[ ${#FAILED[@]} = 0 ] || echo "== failed: ${FAILED[*]}"
echo "== artifacts in $RUN_DIR/artifacts"
exit $rc
