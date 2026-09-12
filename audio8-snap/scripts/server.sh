#!/bin/bash

set -euo pipefail

engine="$(modelctl show-engine --format=json | jq -r .name)"
exec modelctl run -- "$SNAP/engines/$engine/server"
