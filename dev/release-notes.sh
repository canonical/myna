#!/bin/bash
# Draft release notes from the commits since the last tag, for a human to cut
# down to the highlights that become the tag message (.kb/releasing.md).
#
#   dev/release-notes.sh [<since-tag>]
#
# Features and fixes are listed per scope; tests, CI, build, refactors, docs
# and the developer-only scopes are left out, as no user sees them.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
since=${1:-$(git -C "$repo_root" describe --abbrev=0)}
internal='^(benchmarker|bench|e2e|dev|spread|ci|workshop)$'

section() {
    local type=$1 title=$2
    local lines
    lines=$(git -C "$repo_root" log --reverse --no-merges --format=%s "$since..HEAD" |
        sed -n -E "s/^$type(\(([^)]*)\))?!?: (.*)/\2\t\3/p" |
        awk -F'\t' -v internal="$internal" '$1 !~ internal {
            print "- " ($1 == "" ? "" : $1 ": ") $2 }')
    [[ -n $lines ]] || return 0
    printf '\n## %s\n\n%s\n' "$title" "$lines"
}

echo "Changes since $since"
echo
echo "## Highlights"
echo
echo "- TODO: five to eight user-facing lines, written from the lists below"
section feat Features
section fix Fixes
