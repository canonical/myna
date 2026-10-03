#!/bin/bash
# Submit the hardware smoke to Testflinger, wait for it, fetch its
# artifacts into tests/testflinger/out and exit with its test status.
# Locally after `testflinger-cli login`; in CI with TESTFLINGER_CLIENT_ID and
# TESTFLINGER_SECRET_KEY set. A failed or interrupted run cancels the job so
# it does not hold the machine.
#
# Usage: submit.sh QUEUE
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
QUEUE=${1:?usage: submit.sh QUEUE}
# Inside $HOME: the testflinger-cli snap cannot read /tmp.
WORK=$(mktemp -d -p "$HERE")
trap 'rm -rf "$WORK"' EXIT

tar -czf "$WORK/smoke.tgz" -C "$HERE" dut.sh -C ../spread/adapter-smoke smoke.sh fixture.wav
sed "s/__QUEUE__/$QUEUE/" "$HERE/job.yaml" > "$WORK/job.yaml"
JOB=$(testflinger-cli submit --quiet --attachments-relative-to "$WORK" "$WORK/job.yaml")
echo "job $JOB on $QUEUE"
trap 'testflinger-cli cancel "$JOB" || true; rm -rf "$WORK"' EXIT

# poll exits 0 whatever the job did; the status is in the results.
testflinger-cli poll "$JOB"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$HERE/out"
testflinger-cli artifacts --filename "$HERE/out/artifacts.tgz" "$JOB" \
    && tar -xzf "$HERE/out/artifacts.tgz" -C "$HERE/out" || true
STATUS=$(testflinger-cli results "$JOB" | jq -r '.test_status // "none"')
echo "test_status: $STATUS"
[ "$STATUS" = 0 ]
