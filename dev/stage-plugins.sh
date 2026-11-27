#!/bin/bash
# Stage every plugin's model files into <snap-dir>/components/ so
# `snapcraft pack` can ship them as snap components.
#
#   dev/stage-plugins.sh <snap-dir>
#
# The plan comes from <snap-dir>/plugins/*.toml through dev/gen-plugins.py
# --list-files. A file with a url is fetched (Remote Settings CDN, cached
# under ~/.cache/myna/models/<snap>); one without is copied from the [local]
# directory named by its environment variable. The cache keys a file by its
# sha256 (<cache>/<sha256>/<name>), because pairs reuse names (zh-Hans and
# zh-Hant both ship model.zhen...); one cached under its bare name by the older
# layout is linked in, not downloaded again. Either way it must match its
# pinned sha256 or staging fails. The generated manifest and NOTICE, and the
# licence text, are copied on every run, pin current or not: a manifest edited
# after the weights were staged must reach the pack.
set -euo pipefail

snap_dir="$(cd "$1" && pwd)"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source-path=SCRIPTDIR source=model-pin.sh
. "$repo_root/dev/model-pin.sh"

plan="$("$repo_root/dev/gen-plugins.py" --list-files "$snap_dir")"

verified() { # <file> <sha256>
    [ -f "$1" ] && [ "$(sha256sum "$1" | cut -d' ' -f1)" = "$2" ]
}

stage_plugin() { # <snap> <component> <name> <pin> <license file> <env> <default>
    local snap=$1 component=$2 name=$3 pin=$4 license_file=$5 env=$6 default=$7
    local out="$snap_dir/components/$component"
    local cache="${XDG_CACHE_HOME:-$HOME/.cache}/myna/models/$snap"
    local files
    files="$(awk -F'\t' -v c="$component" '$1 == "F" && $2 == c' <<<"$plan")"

    install_text_files() {
        install -m 0644 "$snap_dir/models/$name/"* "$out/"
        install -m 0644 "$license_file" "$out/LICENSE"
    }

    if pin_is_current "$out" "$pin"; then
        install_text_files
        echo "model already present at $out - skipping"
        return 0
    fi

    local src="" file sha url
    if [ "$env" != "-" ]; then
        src="${!env:-${default/#\~/$HOME}}"
    fi
    while IFS=$'\t' read -r _ _ file sha url; do
        if [ "$url" = "-" ]; then
            verified "$src/$file" "$sha" || {
                echo "error: $src/$file is not the pinned $pin ($sha)" >&2
                [ "$env" = "-" ] || echo "       set $env to the staged directory" >&2
                return 1
            }
            continue
        fi
        local cached="$cache/$sha/$file"
        mkdir -p "$cache/$sha"
        if ! [ -f "$cached" ] && verified "$cache/$file" "$sha"; then
            ln "$cache/$file" "$cached" 2>/dev/null || cp -a "$cache/$file" "$cached"
        fi
        verified "$cached" "$sha" || {
            curl --fail --silent --show-error --location --retry 5 \
                --output "$cached.part" "$url"
            mv "$cached.part" "$cached"
        }
        verified "$cached" "$sha" || {
            echo "error: $file does not match its pinned sha256" >&2
            rm -f "$cached"
            return 1
        }
    done <<<"$files"

    rm -rf "$out"
    mkdir -p "$out"
    while IFS=$'\t' read -r _ _ file sha url; do
        local from="$src/$file"
        [ "$url" = "-" ] || from="$cache/$sha/$file"
        cp -al "$from" "$out/$file" 2>/dev/null || cp -a "$from" "$out/$file"
    done <<<"$files"
    install_text_files
    pin_stamp "$out" "$pin"
    echo "component ready at $out"
}

while IFS=$'\t' read -r kind snap component name pin license_file env default; do
    [ "$kind" = P ] || continue
    stage_plugin "$snap" "$component" "$name" "$pin" "$license_file" "$env" "$default"
done <<<"$plan"
