#!/bin/bash
# Submit one suite of the lab run at HEAD to Testflinger, wait for it, fetch its
# artifacts into tests/testflinger/out/SUITE and exit with its test status. The
# machine clones HEAD, so it must be pushed.
# Locally after `testflinger-cli login`; in CI with TESTFLINGER_CLIENT_ID and
# TESTFLINGER_SECRET_KEY set. A failed or interrupted run cancels the job so
# it does not hold the machine.
#
# Usage: submit.sh QUEUE accuracy|continuity
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
QUEUE=${1:?usage: submit.sh QUEUE SUITE}
SUITE=${2:?usage: submit.sh QUEUE SUITE}
# Continuity needs resolute: noble's realtime portal cannot map a snap's
# pids, so capture there is never realtime.
case $SUITE in
    accuracy) DISTRO=noble ;;
    continuity) DISTRO=resolute ;;
    *) echo "unknown suite $SUITE" >&2; exit 2 ;;
esac
# Inside $HOME: the testflinger-cli snap cannot read /tmp.
WORK=$(mktemp -d -p "$HERE")
trap 'rm -rf "$WORK"' EXIT

COMMIT=$(git -C "$HERE" rev-parse HEAD)
sed -e "s/__QUEUE__/$QUEUE/" -e "s/__DISTRO__/$DISTRO/" -e "s/__SUITE__/$SUITE/g" \
    -e "s/__COMMIT__/$COMMIT/" "$HERE/job.yaml" > "$WORK/job.yaml"
JOB=$(testflinger-cli submit --quiet "$WORK/job.yaml")
echo "job $JOB: $SUITE on $QUEUE ($DISTRO)"
trap 'testflinger-cli cancel "$JOB" || true; rm -rf "$WORK"' EXIT

# poll exits 0 whatever the job did; the status is in the results.
testflinger-cli poll "$JOB"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$HERE/out/$SUITE"
if testflinger-cli artifacts --filename "$HERE/out/$SUITE/artifacts.tgz" "$JOB"; then
    tar -xzf "$HERE/out/$SUITE/artifacts.tgz" -C "$HERE/out/$SUITE" || true
fi
STATUS=$(testflinger-cli results "$JOB" | jq -r '.test_status // "none"')
echo "test_status: $STATUS"
[ "$STATUS" = 0 ]
