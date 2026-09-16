#!/bin/bash
# Print the version a snap packed from this checkout adopts, in the form
# snapcraft's `version: git` gives: 0+git.<sha> with no annotated tag behind
# HEAD, <tag>+git<n>.<sha> past one, the bare tag on it. That keyword cannot
# run here because build instances mount only the snap directory, not .git.
#
# Usage: dev/snap-version.sh [<path>...]
# The paths, relative to the repository root, are what the snap packs: -dirty
# is appended only when they differ from HEAD. Without paths it never is.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

if described=$(git -C "$repo_root" describe 2>/dev/null); then
    if [[ $described =~ ^(.+)-([0-9]+)-g([0-9a-f]+)$ ]]; then
        version="${BASH_REMATCH[1]}+git${BASH_REMATCH[2]}.${BASH_REMATCH[3]}"
    else
        version=$described
    fi
else
    version="0+git.$(git -C "$repo_root" rev-parse --short HEAD)"
fi

if (($#)) && ! git -C "$repo_root" diff --quiet HEAD -- "$@"; then
    version+="-dirty"
fi
echo "$version"
