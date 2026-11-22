#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# What to stage, and the sha256 each file must match, is in plugins/*.toml.
# model-disfluency-en is our own fine-tune and has no public home yet: it is
# copied from the directory the training run staged (MYNA_TAGGER_SRC).
set -euo pipefail

exec "$(dirname "${BASH_SOURCE[0]}")/../../dev/stage-plugins.sh" "$(dirname "${BASH_SOURCE[0]}")/.."
