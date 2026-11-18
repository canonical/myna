#!/bin/sh
# Succeed if <commit> was released to a candidate branch by a manual snaps.yml
# run, which is what a release tag on it requires. Run in a full clone.
#
#   dev/release-candidate.sh <owner/repo> <commit>
#
# The mark of such a run is its upgrade-e2e job: it runs only after every
# snap was released to a channel other than edge. The candidate may be an
# ancestor that differs by debian/changelog alone, since the release commit
# that writes it comes after the candidate and no snap packs it.
set -eu
repo=$1
commit=$2
changelog=myna-config-deb/debian/changelog

gh api --paginate "repos/$repo/actions/workflows/snaps.yml/runs?event=workflow_dispatch&per_page=100" \
    --jq '.workflow_runs[] | "\(.id) \(.head_sha)"' |
    while read -r run sha; do
        git merge-base --is-ancestor "$sha" "$commit" 2>/dev/null || continue
        [ -z "$(git diff --name-only "$sha" "$commit" -- . ":(exclude)$changelog")" ] || continue
        gh api "repos/$repo/actions/runs/$run/jobs?per_page=100" \
            --jq '.jobs[] | select(.name == "upgrade-e2e") | .conclusion' | grep -qx success || continue
        echo "candidate: https://github.com/$repo/actions/runs/$run"
        exit 3
    done && status=0 || status=$?
[ "$status" = 3 ] && exit 0
[ "$status" = 0 ] || exit "$status"
echo "::error::$commit was never released as a candidate. Run: gh workflow run snaps.yml --ref main -f publish=true -f channel=edge/rc1" >&2
exit 1
