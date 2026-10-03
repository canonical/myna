#!/bin/bash
# The device half of the Testflinger nightly, run as root from a checkout on
# the lab machine, everything from latest/edge: the adapter smoke through
# the Myna client, then the FLEURS smoke tier through each backend, gated
# per language. Results land in out/.
set -eux
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$HERE/out"
cd "$HERE/out"

# `myna-bench download-corpus --preset fleurs-smoke`, published once: 33 MB
# instead of the 3.5 GB of per-language archives it is cut from.
TIER=https://github.com/canonical/myna/releases/download/fleurs-smoke-v1/fleurs-smoke.tar.gz
TIER_SHA256=f1e1cbb8133de23e41497a683cbc0ff43de6c78d21e65d60cdae03abf04695bb
curl -fsSL "$TIER" -o fleurs-smoke.tar.gz
echo "$TIER_SHA256  fleurs-smoke.tar.gz" | sha256sum -c
tar -xzf fleurs-smoke.tar.gz

snap set system experimental.user-daemons=true
snap install --edge myna
snap install --edge myna-whisper+model-tiny
SNAP_UNDER_TEST=myna-whisper MODELCTL=myna-whisper.whisper MODEL=tiny \
    "$HERE/../spread/adapter-smoke/smoke.sh"

apt-get install -y -q python3-venv
"$HERE/../../dev/build-bench.sh"
BENCH="python3 $HERE/../../myna-bench.pyz"
$BENCH run --config "$HERE/fleurs.yaml"
$BENCH summarize --in fleurs.jsonl --no-ci --by-category --gate "$HERE/ceilings.yaml"
