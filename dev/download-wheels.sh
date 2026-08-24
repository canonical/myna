#!/bin/bash
# Pre-download a snap's pip dependencies into <snap>/wheels/cache/ so
# `snapcraft pack` resolves them locally instead of over the network.
#
# Why: snapcraft builds in a throwaway VM, so pip's HTTP cache is discarded on
# every run and each rebuild re-fetches the lot. The CUDA components are the
# painful case (whisper's cuBLAS+cuDNN are ~1.3 GB; nemotron's is larger), but
# every snap pays it. This caches on the host, where it survives.
#
# Opt-in and non-breaking: the recipes set PIP_FIND_LINKS at this directory but
# never --no-index, so a missing or incomplete cache just falls back to PyPI.
#
#   ./dev/download-wheels.sh whisper-snap      # one snap
#   ./dev/download-wheels.sh                   # every snap that pip-installs
#
# Requirements are read out of each snapcraft.yaml rather than restated here,
# so this cannot drift from what the build actually installs. Output is
# gitignored; delete wheels/cache to refresh.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

command -v uv >/dev/null || { echo "uv is required (resolves for the build's python)" >&2; exit 1; }

snap_dirs=("$@")
if [ ${#snap_dirs[@]} -eq 0 ]; then
    # A glob rather than `ls -d`, which shellcheck flags and which would
    # mangle a name with whitespace in it.
    mapfile -t snap_dirs < <(cd "$repo_root" && printf '%s\n' *-snap)
fi

for snap_dir in "${snap_dirs[@]}"; do
    recipe="$repo_root/$snap_dir/snap/snapcraft.yaml"
    [ -f "$recipe" ] || { echo "no snapcraft.yaml in $snap_dir" >&2; exit 1; }

    wheels="$repo_root/$snap_dir/wheels"
    specs=$(python3 - "$recipe" <<'PY'
import shlex, sys, yaml

def pip_operands(script):
    """Package specs from `pip install` lines, honouring \\ continuations."""
    out, logical = [], []
    for raw in script.splitlines():
        logical.append(raw)
        if raw.rstrip().endswith("\\"):
            continue
        line = " ".join(l.rstrip().rstrip("\\") for l in logical)
        logical = []
        try:
            words = shlex.split(line)
        except ValueError:
            continue
        if not any(w in ("pip", "pip3") for w in words[:1]) or "install" not in words[:2]:
            continue
        for word in words[2:]:
            # Flags, their values, and anything shell-expanded (a --target= or
            # --prefix path) are not package specs.
            if word.startswith("-") or "$" in word:
                continue
            out.append(word)
    return out

recipe = yaml.safe_load(open(sys.argv[1], encoding="utf-8"))
specs = []
for part in (recipe.get("parts") or {}).values():
    if not isinstance(part, dict):
        continue
    specs += [str(p) for p in (part.get("python-packages") or [])]
    # The GPU components pip-install by hand in override-build; read the
    # operands off those lines so both mechanisms stay in sync with the recipe.
    specs += pip_operands(part.get("override-build") or "")
print("\n".join(dict.fromkeys(specs)))
PY
)
    if [ -z "$specs" ]; then
        echo "$snap_dir: no pip packages, skipping"
        continue
    fi
    # The staged myna wheel is the marker for "this snap installs out of
    # wheels/". Asked for explicitly, a missing one is a mistake worth stopping
    # for; in a sweep over every snap it just means this one is not a candidate.
    if ! compgen -G "$wheels"/myna-*.whl > /dev/null; then
        if [ $# -gt 0 ]; then
            echo "$snap_dir: no myna wheel in wheels/ - run $snap_dir/dev/prepare.sh first" >&2
            exit 1
        fi
        echo "$snap_dir: no staged wheels, skipping"
        continue
    fi

    # Resolve for the *build* interpreter, not this host's: core24 builds on
    # python 3.12, and wheels fetched for a 3.14 host could never install there.
    base=$(python3 -c "import sys,yaml; print(yaml.safe_load(open(sys.argv[1]))['base'])" "$recipe")
    case "$base" in
        core24) py=3.12 ;;
        core22) py=3.10 ;;
        *) echo "$snap_dir: unknown base $base - add its python version" >&2; exit 1 ;;
    esac

    echo "== $snap_dir (base $base, python $py)"
    printf '   %s\n' "${specs//$'\n'/$'\n'   }"
    mkdir -p "$wheels/cache"

    # Two steps, because pip alone cannot do this. Asking pip to resolve for
    # another interpreter forces --only-binary=:all:, so one sdist-only
    # dependency (funasr pulls jieba) fails the entire run. uv resolves from
    # metadata without building anything, giving a pinned set we then fetch one
    # at a time - so the packages that do have wheels still get cached.
    pinned=$(cd "$wheels" && echo "$specs" | uv pip compile \
        --python-version "$py" --no-header --no-annotate - \
        | grep -vE '^[[:space:]]*#|^[[:space:]]*$|^[./]')

    # pip's stdout is left alone: the CUDA wheels are hundreds of MB each and
    # its progress bar is the only sign the run is alive. Errors go to stderr
    # and are dropped, because a missing wheel is expected and summarised below.
    total=$(echo "$pinned" | wc -w)
    cached=0
    n=0
    skipped=""
    for pkg in $pinned; do
        n=$((n + 1))
        echo "   [$n/$total] $pkg"
        if (cd "$wheels" && pip3 download --no-deps --dest cache \
                --only-binary=:all: --python-version "$py" "$pkg") 2>/dev/null; then
            cached=$((cached + 1))
        else
            skipped="$skipped $pkg"
        fi
    done

    echo "   cached $cached of $total ($(du -sh "$wheels/cache" | cut -f1))"
    # Never fatal: PIP_FIND_LINKS is additive, so anything missing here is just
    # fetched from PyPI at build time, exactly as it is today.
    [ -n "$skipped" ] && echo "   no python-$py wheel, left to the build:$skipped"

done
