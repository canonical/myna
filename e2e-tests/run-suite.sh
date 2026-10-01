#!/usr/bin/env bash
# Run one e2e suite against a provisioned VM: revert it to the snapshot the
# suite declares (a `# snapshot: NAME` header), boot, run the suite, collect
# artifacts. The same entry point runs in CI.
#
# Usage: run-suite.sh --release noble|resolute|stonking --suite NAME [--no-build] [--keep-running]
# Suites live in suites/ (e.g. --suite onboarding-full).
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=vm/lib.sh
source "$HERE/vm/lib.sh"

REL= SUITE= BUILD=1 KEEP=
while [ $# -gt 0 ]; do
    case $1 in
        --release) REL=$2; shift 2 ;;
        --suite) SUITE=$2; shift 2 ;;
        --no-build) BUILD=0; shift ;;
        --keep-running) KEEP=1; shift ;;
        -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
case $REL in noble|resolute|stonking) ;; *) echo "--release noble|resolute|stonking" >&2; exit 2 ;; esac
SUITE_FILE=$HERE/suites/$SUITE.sh
[ -f "$SUITE_FILE" ] || { echo "no suite $SUITE ($SUITE_FILE)" >&2; exit 2; }

VM=$(vm_name "$REL")
SNAP=$(sed -n 's/^# snapshot: //p' "$SUITE_FILE" | head -1)
SNAP=${SNAP:-bare}
SNAPS=$(virsh snapshot-list "$VM" --name 2>/dev/null)
grep -qx "$SNAP" <<< "$SNAPS" \
    || { echo "$VM lacks snapshot $SNAP; run vm/provision.sh --release $REL" >&2; exit 1; }

ARTIFACTS=$RUN_DIR/artifacts/$REL-$SUITE-$(date +%Y%m%d-%H%M%S)
mkdir -p "$ARTIFACTS"
echo "== reverting $VM to $SNAP"
[ "$(virsh domstate "$VM")" = "shut off" ] || shutdown_vm "$VM"
virsh snapshot-revert "$VM" "$SNAP"
IP=$(start_vm "$VM")
wait_ready "$IP"
echo "== $VM at $IP, running $SUITE (artifacts: $ARTIFACTS)"

export REL IP T=$HERE/tools ARTIFACTS SUITE_LIB=$HERE/suites/lib.sh
BUILD_FLAG=(); [ $BUILD = 0 ] && BUILD_FLAG=(--no-build)
rc=0
MYNA_SHOT_BUILD_FLAGS="${BUILD_FLAG[*]}" bash "$SUITE_FILE" || rc=$?

vm_ssh "$IP" 'sudo journalctl -b --no-pager -o short-precise' > "$ARTIFACTS/journal.log" 2>/dev/null || true
[ -z "$KEEP" ] && shutdown_vm "$VM" || true
echo "== $SUITE rc=$rc; artifacts in $ARTIFACTS"
exit $rc
