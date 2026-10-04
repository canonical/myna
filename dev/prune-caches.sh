#!/bin/bash
# Delete GitHub Actions caches that a newer copy of the same cache supersedes.
#
#   dev/prune-caches.sh <owner/repo> [--dry-run]
#
# canonical/launch-workshop saves its cache under <key>-<run id>-<attempt> on
# every run and restores by prefix, so each run adds ~350 MB; past the 10 GB
# quota GitHub evicts least recently used first, the spread image included.
# This keeps the newest entry per key and ref. Delete it once
# canonical/launch-workshop#362 lands and we adopt its cache-key input.
set -euo pipefail

repo=$1
dry_run=${2:-}

gh cache list -R "$repo" --limit 1000 --json id,key,ref,lastAccessedAt,sizeInBytes \
    --jq 'map(.group = "\(.key | sub("-[0-9]+-[0-9]+$"; ""))@\(.ref)")
          | group_by(.group)[]
          | sort_by(.lastAccessedAt) | reverse | .[1:][]
          | "\(.id) \(.sizeInBytes / 1048576 | floor) MB \(.ref) \(.key)"' |
    while read -r id rest; do
        echo "delete $id $rest"
        [[ $dry_run == --dry-run ]] || gh cache delete "$id" -R "$repo"
    done
