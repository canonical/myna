#!/usr/bin/env bash
# changed-tags.sh - the e2e tags a diff touches, for `make e2e-changed`.
#
# Usage: changed-tags.sh [BASE]   (default origin/main)
#
# Prints one tag per line, uniq. The mapping is deliberately coarse: a path
# says which part of the product it is, and a part is a handful of suites. A
# change to none of these (docs, packaging, the server) prints nothing, and
# `make e2e-changed` runs nothing.
set -uo pipefail

BASE=${1:-origin/main}
REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
cd "$REPO" || exit 1

# Read the changed paths once; a tag is a substring test over them.
changes=$(git diff --name-only "$BASE"...HEAD 2>/dev/null || git diff --name-only HEAD)
[ -n "$changes" ] || exit 0

tag() { # TAG PATTERN...
    local tag=$1
    shift
    local path pattern
    for path in $changes; do
        for pattern in "$@"; do
            # shellcheck disable=SC2254 # the patterns are globs on purpose
            case $path in $pattern) echo "$tag"; return ;; esac
        done
    done
}

tag processing 'client/myna-config/src/processing_*.rs' \
    'client/myna-config/src/chains_view.rs' \
    'client/myna-config/src/processing_steps.rs' \
    'client/myna-config/src/step_catalog.rs'
tag settings 'client/myna-config/src/app.rs' 'client/myna-config/src/active_backend.rs' \
    'client/myna-config/src/backend_*.rs' 'client/myna-config/src/chain_notice.rs' \
    'client/myna-config/src/diagnostics.rs' 'client/myna-config/src/domain.rs' \
    'client/myna-config/src/installed_models.rs' 'client/myna-config/src/list_editor_ui.rs' \
    'client/myna-config/src/myna_settings.rs' 'client/myna-config/src/models_ui.rs' \
    'client/myna-config/src/operation_gate.rs' 'client/myna-config/src/past_dictations_ui.rs' \
    'client/myna-config/src/shortcut*.rs' 'client/myna-config/src/snap_*.rs' \
    'client/myna-config/src/sound_preview.rs' 'client/myna-config/src/subpage.rs' \
    'client/myna-config/src/ui/*' 'client/myna-config/src/adapters/*' \
    'client/myna-config/src/platform/*' 'client/myna-config/data/*'
tag onboarding 'client/myna-config/src/onboarding*'
tag locales 'client/myna-config/po/*' 'client/myna-config/src/languages.rs'
tag dictation 'client/myna-desktop/*' 'client/myna-orchestrator/*' 'client/myna-audio/*' \
    'client/myna-core/*'
tag 'text-model' 'text-snap/*' 'translation-snap/*'
tag plugins 'text-snap/*' 'translation-snap/*' 'client/myna-config/src/plugin_*.rs'
tag upgrade 'myna-snap/*'
