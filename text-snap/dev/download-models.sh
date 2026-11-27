#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# What to stage, and the sha256 each file must match, is in plugins/*.toml.
# The taggers are our own fine-tunes and have no public home yet: they are
# copied from the directories their training runs staged (MYNA_TAGGER_EN_SRC
# for model-disfluency-en, MYNA_TAGGER_SRC for model-disfluency-fr-de-es).
set -euo pipefail

exec "$(dirname "${BASH_SOURCE[0]}")/../../dev/stage-plugins.sh" "$(dirname "${BASH_SOURCE[0]}")/.."
