#!/bin/bash

set -euo pipefail

# CPU-only snap: don't ask modelctl to score hardware just to find the one
# engine. The service only needs modelctl for config reads below.
exec "$SNAP/engines/cpu/server" "$@"
