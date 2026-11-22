#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# model-translate-en-fr comes from Mozilla's Remote Settings CDN, each file
# checked against the sha256 of its record. model-disfluency-en is our own
# fine-tune and has no public home yet: it is copied from the directory the
# training run staged (MYNA_TAGGER_SRC), and refused unless its files match the
# hashes pinned here.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/myna-text"
mkdir -p "$cache"

# Keep the stamp in step with the pins below: a staged directory from another
# pin is rebuilt rather than trusted.
translate_rev="mozilla firefox-translations en-fr base-memory 2.0"
tagger_rev="myna-disfluency-en l12c"
cdn="https://firefox-settings-attachments.cdn.mozilla.net/main-workspace/translations-models"

# name uuid.extension sha256, from the Remote Settings records of
# translations-models with fromLang=en, toLang=fr, version 2.0.
translate_files="
model.enfr.intgemm.alphas.bin e0d7f3ee-163a-4b0a-8e44-8b208b39477d.bin 6322e296d4fecfe395a8d5723da4ec37ecbe6d7613bb1dfcf4b28e2a47498b68
lex.50.50.enfr.s2t.bin 293d1823-ff05-48c9-837b-cff8fd33f197.bin 2585ed98d3af0bc949865aedeb390493d591f56870814376e73e4144c41ed059
vocab.enfr.spm c7c1bcf3-71f6-425a-a6ed-403bd9a0a759.spm 783abf3abe075afdf8d85d233994bef2c3a064e935ab1bed946820aff6ac002a
"

tagger_files="
tagger.onnx 66f63787da3fe7f63c7288250b872cf977d77b4a9019409cc723f69956054c3d
tokenizer.json 851ca67100d372ca3ae031a6abd168f53489eebfd7d89523f35c5c9b4d372c3c
"

verified() { # <file> <sha256>
    [ -f "$1" ] && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]
}

stage_translate() {
    local out="$snap_dir/components/model-translate-en-fr"
    if pin_is_current "$out" "$translate_rev"; then
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
    install -m 0644 "$snap_dir/models/translate-en-fr/"{myna-text-model.json,config.yml,NOTICE} "$out/"
    install -m 0644 /usr/share/common-licenses/MPL-2.0 "$out/LICENSE"
    pin_stamp "$out" "$translate_rev"
    echo "component ready at $out"
}

stage_tagger() {
    local out="$snap_dir/components/model-disfluency-en"
    local src="${MYNA_TAGGER_SRC:-$HOME/myna-processing-graphs/artifacts/tagger/stage/myna-disfluency-en}"
    if pin_is_current "$out" "$tagger_rev"; then
        echo "model already present at $out - skipping"
        return 0
    fi
    local name sha
    while read -r name sha; do
        [ -n "$name" ] || continue
        verified "$src/$name" "$sha" || {
            echo "error: $src/$name is not the pinned $tagger_rev ($sha)" >&2
            echo "       set MYNA_TAGGER_SRC to the staged myna-disfluency-en directory" >&2
            return 1
        }
    done <<<"$tagger_files"
    rm -rf "$out"
    mkdir -p "$out"
    while read -r name _; do
        [ -n "$name" ] || continue
        cp -al "$src/$name" "$out/$name" 2>/dev/null || cp -a "$src/$name" "$out/$name"
    done <<<"$tagger_files"
    install -m 0644 "$snap_dir/models/disfluency-en/"{myna-text-model.json,NOTICE} "$out/"
    install -m 0644 "$src/LICENSE" "$out/LICENSE"
    pin_stamp "$out" "$tagger_rev"
    echo "component ready at $out"
}

stage_translate
stage_tagger
