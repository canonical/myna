#!/bin/bash
# Print the version of everything built from this checkout - the snaps, the
# deb, the client binaries and the extension - from the last annotated tag
# v<X.Y.Z>: X.Y.Z on it, X.Y.Z+git<n>.<sha> past it, 0+git.<sha> with none.
# This is snapcraft's `version: git` without the v, which Debian rejects.
# That keyword cannot run here: build instances mount only the snap directory.
#
# Usage: dev/version.sh [<path>...]
# The paths, relative to the repository root, are what the build packs: -dirty
# is appended only when they differ from HEAD. Without paths it never is.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

if described=$(git -C "$repo_root" describe --match 'v[0-9]*' 2>/dev/null); then
    described=${described#v}
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
