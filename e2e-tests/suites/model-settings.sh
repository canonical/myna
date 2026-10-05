#!/usr/bin/env bash
# snapshot: installed
# A Model-tab backend setting applies through one pkexec prompt: a dismissed
# prompt (pkexec 126) reverts the row silently and writes nothing; an
# allowed one lands in snapd's config for the connected backend.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

P=model-settings-$REL
# The connected backend's snap name (myna-whisper, myna-parakeet, ...).
BACKEND=$(on 'snap connections myna | sed -n "s/^.*myna:backend[[:space:]]\+\(myna-[a-z]*\):provider.*$/\1/p"')
[ -n "$BACKEND" ] || { echo "no backend connected" >&2; exit 1; }
echo "connected backend: $BACKEND"
OLD=$(on "sudo snap get -d $BACKEND | jq -r 'first(.. .\"sleep-idle-seconds\"? // empty)'")
echo "old sleep-idle-seconds = $OLD"
NEW=450
[ "$OLD" = 450 ] && NEW=425

# "Unload when idle (seconds)" is the Runtime entry row for package.sleep-idle-seconds.
ROW="Unload when idle (seconds)"

# Cancel: the row reverts silently, snapd keeps the old value.
shot --polkit cancel \
    wait:4 tab:Model wait:2 \
    "scrollat:550,700#8" wait:1 \
    "click:$ROW@text" "key:ctrl+a" "type:$NEW" key:Return wait:4 shot:"$P"-01-cancelled.png \
    || { echo "cancel run failed" >&2; exit 1; }
assert_on "cancel wrote nothing" \
    "sudo snap get -d $BACKEND | jq -e '[.. .\"sleep-idle-seconds\"? // empty] | index($NEW) == null' >/dev/null"

# Allow: the value lands in snapd.
shot --polkit allow --monitor "$P" \
    wait:4 tab:Model wait:2 \
    "scrollat:550,700#8" wait:1 \
    "click:$ROW@text" "key:ctrl+a" "type:$NEW" key:Return wait:5 shot:"$P"-02-applied.png \
    || { echo "allow run failed" >&2; exit 1; }
assert_on "new value landed in snapd" \
    "sudo snap get -d $BACKEND | jq -e '[.. .\"sleep-idle-seconds\"? // empty] | index($NEW) != null' >/dev/null"
suite_status
