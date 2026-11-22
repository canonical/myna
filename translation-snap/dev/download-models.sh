#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# model-translate-en-fr comes from Mozilla's Remote Settings CDN, each file
# checked against the sha256 of its record.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/myna-translation"
mkdir -p "$cache"

# Keep the stamp in step with the pins below: a staged directory from another
# pin is rebuilt rather than trusted.
translate_rev="mozilla firefox-translations en-fr base-memory 2.0"
cdn="https://firefox-settings-attachments.cdn.mozilla.net/main-workspace/translations-models"

# name uuid.extension sha256, from the Remote Settings records of
# translations-models with fromLang=en, toLang=fr, version 2.0.
translate_files="
model.enfr.intgemm.alphas.bin e0d7f3ee-163a-4b0a-8e44-8b208b39477d.bin 6322e296d4fecfe395a8d5723da4ec37ecbe6d7613bb1dfcf4b28e2a47498b68
lex.50.50.enfr.s2t.bin 293d1823-ff05-48c9-837b-cff8fd33f197.bin 2585ed98d3af0bc949865aedeb390493d591f56870814376e73e4144c41ed059
vocab.enfr.spm c7c1bcf3-71f6-425a-a6ed-403bd9a0a759.spm 783abf3abe075afdf8d85d233994bef2c3a064e935ab1bed946820aff6ac002a
"

verified() { # <file> <sha256>
    [ -f "$1" ] && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]
}

# The small files come from the tree and are copied on every run, pin current
# or not: a manifest edited after the weights were staged must reach the pack.
translate_text_files() { # <component dir>
    install -m 0644 "$snap_dir/models/translate-en-fr/"{myna-text-model.json,config.yml,NOTICE} "$1/"
    install -m 0644 /usr/share/common-licenses/MPL-2.0 "$1/LICENSE"
}

stage_translate() {
    local out="$snap_dir/components/model-translate-en-fr"
    if pin_is_current "$out" "$translate_rev"; then
        translate_text_files "$out"
        echo "model already present at $out - skipping"
        return 0
    fi
    local name remote sha
    while read -r name remote sha; do
        [ -n "$name" ] || continue
        verified "$cache/$name" "$sha" || {
            curl --fail --silent --show-error --location --retry 5 \
                --output "$cache/$name.part" "$cdn/$remote"
            mv "$cache/$name.part" "$cache/$name"
        }
        verified "$cache/$name" "$sha" || {
            echo "error: $name does not match its pinned sha256" >&2
            rm -f "$cache/$name"
            return 1
        }
    done <<<"$translate_files"
    rm -rf "$out"
    mkdir -p "$out"
    while read -r name _ _; do
        [ -n "$name" ] || continue
        cp -al "$cache/$name" "$out/$name" 2>/dev/null || cp -a "$cache/$name" "$out/$name"
    done <<<"$translate_files"
    translate_text_files "$out"
    pin_stamp "$out" "$translate_rev"
    echo "component ready at $out"
}

stage_translate
