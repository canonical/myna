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

snap_dirs=("$@")
if [ ${#snap_dirs[@]} -eq 0 ]; then
    mapfile -t snap_dirs < <(cd "$repo_root" && ls -d ./*-snap | sed 's|^\./||')
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

    # Resolve for the *build* interpreter, not this host's. core24 builds on
    # python 3.12; a bare `pip download` here fetches wheels for whatever the
    # host runs (3.14 at the time of writing), which can never install in the
    # build. --python-version implies --only-binary, so an sdist-only
    # dependency will fail loudly rather than cache something unusable.
    base=$(python3 -c "import sys,yaml; print(yaml.safe_load(open(sys.argv[1]))['base'])" "$recipe")
    case "$base" in
        core24) py=3.12 ;;
        core22) py=3.10 ;;
        *) echo "$snap_dir: unknown base $base - add its python version" >&2; exit 1 ;;
    esac

    echo "== $snap_dir (base $base, python $py)"
    echo "$specs" | sed 's/^/   /'
    mkdir -p "$wheels/cache"
    # Run from wheels/ so the relative ./myna-*.whl specs resolve.
    (cd "$wheels" && echo "$specs" | xargs pip3 download \
        --only-binary=:all: --python-version "$py" --dest cache)
    echo "   cached $(find "$wheels/cache" -name '*.whl' | wc -l) wheels ($(du -sh "$wheels/cache" | cut -f1))"
done
