#!/usr/bin/env bash
# Print one hash for the state of PATHSPEC... in the working tree: the
# files git tracks there, uncommitted changes and untracked files. Two
# builds from the same stamp built the same source, whatever HEAD says.
# Pathspec magic works, so ':!client/*/tests' leaves the tests out.
#
# Usage: tree-stamp.sh PATHSPEC...
set -euo pipefail
cd "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
{
    git ls-files --stage -- "$@"
    git diff HEAD -- "$@"
    git ls-files --others --exclude-standard -- "$@" | while read -r file; do
        echo "$file"; sha256sum < "$file"
    done
} | sha256sum | cut -d' ' -f1
