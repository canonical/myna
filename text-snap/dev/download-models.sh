#!/bin/bash
# Stage the model weights into components/ so `snapcraft pack` can ship them as
# snap model components (see the model-components part in snap/snapcraft.yaml).
#
#   ./dev/download-models.sh
#
# model-disfluency-en is our own fine-tune and has no public home yet: it is
# copied from the directory the training run staged (MYNA_TAGGER_SRC), and
# refused unless its files match the hashes pinned here.
set -euo pipefail

snap_dir="$(cd "$(dirname "$0")/.." && pwd)"
repo_root="$(dirname "$snap_dir")"
# shellcheck source-path=SCRIPTDIR/../.. source=dev/model-pin.sh
. "$repo_root/dev/model-pin.sh"

# Keep the stamp in step with the pins below: a staged directory from another
# pin is rebuilt rather than trusted.
tagger_rev="myna-disfluency-en l12c"
tagger_files="
tagger.onnx 66f63787da3fe7f63c7288250b872cf977d77b4a9019409cc723f69956054c3d
tokenizer.json 851ca67100d372ca3ae031a6abd168f53489eebfd7d89523f35c5c9b4d372c3c
"

verified() { # <file> <sha256>
    [ -f "$1" ] && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]
}

# The small files come from the tree and are copied on every run, pin current
# or not: a manifest edited after the weights were staged must reach the pack.
tagger_text_files() { # <component dir>
    install -m 0644 "$snap_dir/models/disfluency-en/"{myna-text-model.json,NOTICE} "$1/"
    install -m 0644 /usr/share/common-licenses/Apache-2.0 "$1/LICENSE"
}

stage_tagger() {
    local out="$snap_dir/components/model-disfluency-en"
    local src="${MYNA_TAGGER_SRC:-$HOME/myna-processing-graphs/artifacts/tagger/stage/myna-disfluency-en}"
    if pin_is_current "$out" "$tagger_rev"; then
        tagger_text_files "$out"
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
    tagger_text_files "$out"
    pin_stamp "$out" "$tagger_rev"
    echo "component ready at $out"
}

stage_tagger
