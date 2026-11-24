#!/bin/bash
# Run a command and report its wall time: a `[timing]` line on stderr, and in
# CI a row in the step's summary, so every job page lists where its time went.
#
#   dev/timed.sh CMD [ARG...]                 time CMD, exit with its status
#   dev/timed.sh --record LABEL SECONDS RC    report a duration timed elsewhere
#
# The label of a timed command is the command itself, less `workshop run`.
set -uo pipefail

report() {
    local label=$1 secs=$2 rc=$3
    printf '[timing] %s: %ss (exit %s)\n' "$label" "$secs" "$rc" >&2
    [ -n "${GITHUB_STEP_SUMMARY:-}" ] || return 0
    grep -qx '| timed | seconds | exit |' "$GITHUB_STEP_SUMMARY" 2>/dev/null ||
        printf '\n| timed | seconds | exit |\n|---|---:|---:|\n' >> "$GITHUB_STEP_SUMMARY"
    printf '| %s%s%s | %s | %s |\n' '`' "$label" '`' "$secs" "$rc" >> "$GITHUB_STEP_SUMMARY"
}

if [ "${1:-}" = --record ]; then
    report "$2" "$3" "$4"
    exit 0
fi

start=$EPOCHREALTIME
"$@"
rc=$?
end=$EPOCHREALTIME
label="$*"
label=${label#workshop run }
report "$label" "$(LC_ALL=C awk -v a="$start" -v b="$end" 'BEGIN { printf "%.1f", b - a }')" "$rc"
exit "$rc"
