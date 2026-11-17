#!/bin/bash

set -euo pipefail

# `status` reads the active engine from modelctl's cache; `engine` and
# `show-engine` re-score the hardware, which fails without hardware-observe -
# the state every sideload starts in.
engine="$(modelctl status --format=json | jq -r .engine)"
exec modelctl run -- "$SNAP/engines/$engine/server"
