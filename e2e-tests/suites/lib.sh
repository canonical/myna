# shellcheck shell=bash
# Helpers every suite gets by sourcing "$SUITE_LIB". run-suite.sh exports
# REL, IP, T (tools dir) and ARTIFACTS before invoking a suite.
#
#   shot [shot.sh options] [steps]   drive the app, PNGs land in $ARTIFACTS
#   on "remote command"              run over ssh as ubuntu, session env set
#   assert "description" cmd...      local assertion, tallied
#   assert_ssh "description" "cmd"   remote assertion, tallied
#   suite_status                     exit 1 if any assertion failed

source "$(cd "$(dirname "${BASH_SOURCE[0]}")/../vm" && pwd)/lib.sh"

FAILED=0

shot() { "$T/shot.sh" --release "$REL" --out "$ARTIFACTS" ${MYNA_SHOT_BUILD_FLAGS:-} "$@"; }

on() {
    vm_ssh "$IP" "export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus; $1"
}

_tally() {
    local desc=$1 rc=$2
    if [ $rc -eq 0 ]; then echo "PASS: $desc"; else echo "FAIL: $desc"; FAILED=1; fi
}

assert() {
    local desc=$1; shift
    "$@" >/dev/null 2>&1
    _tally "$desc" $?
}

assert_ssh() {
    local desc=$1 cmd=$2
    on "$cmd" >/dev/null 2>&1
    _tally "$desc" $?
}

suite_status() {
    [ $FAILED -eq 0 ] || { echo "suite had failures" >&2; exit 1; }
}

# The accessible role GTK reports for plain buttons differs by toolkit
# (noble's 4.14 says "push button", newer say "button").
case $REL in
    noble) BTN="push button" ;;
    *) BTN="button" ;;
esac
