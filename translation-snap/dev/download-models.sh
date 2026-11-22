#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# What to stage, and the sha256 each file must match, is in plugins/*.toml:
# the models come from Mozilla's Remote Settings CDN.
set -euo pipefail

exec "$(dirname "${BASH_SOURCE[0]}")/../../dev/stage-plugins.sh" "$(dirname "${BASH_SOURCE[0]}")/.."
