# shellcheck shell=bash
# Model-pin helpers, shared by the per-snap dev/download-models.sh scripts.
# Source it, don't execute it:  . "$repo_root/dev/model-pin.sh"
#
# Every fetcher pins the upstream revision it stages. Without a pin, `hf
# download <repo>` resolves that repo's current main at fetch time, so the same
# command on two machines - or one machine two months apart - can pack
# different weights under the same snap version. Moving a pin is a commit, and
# that commit is where the snap version gets bumped.
#
# A pin on its own is not enough. Each script's "already present" guard skips
# on a *file existing*, so a checkout staged before the pin moved would keep
# its old weights forever and never say so. These helpers stamp the staged
# directory with the revision it came from and let the guard compare, turning
# drift into a re-fetch instead of a silently stale pack.
#
# The stamp ships inside the model component: provenance travels with the
# weights, so an installed snap can answer which upstream revision it carries.

PIN_STAMP_FILE=UPSTREAM_REVISION

# pin_revision_of <dir> - what <dir> was staged from, empty if unstamped.
pin_revision_of() {
    cat "$1/$PIN_STAMP_FILE" 2>/dev/null || true
}

# pin_is_current <dir> <revision> - true when <dir> was staged at <revision>.
pin_is_current() {
    [ "$(pin_revision_of "$1")" = "$2" ]
}

# pin_stamp <dir> <revision> - record what <dir> was staged from.
pin_stamp() {
    mkdir -p "$1"
    printf '%s\n' "$2" >"$1/$PIN_STAMP_FILE"
}

# pin_check_source <src> <revision> <hint> - guard the MYNA_MODEL_SRC reuse
# path. A local copy that names a *different* revision is the developer
# pointing at the wrong weights; refuse rather than hardlink them in under the
# pinned name. An unstamped copy predates stamping, so it is taken on trust and
# stamped by the caller.
pin_check_source() {
    local src_rev
    src_rev="$(pin_revision_of "$1")"
    if [ -n "$src_rev" ] && [ "$src_rev" != "$2" ]; then
        echo "error: $1 is staged at $src_rev, not the pinned $2" >&2
        echo "       point MYNA_MODEL_SRC at matching weights, or move the pin in $3" >&2
        return 1
    fi
    return 0
}
