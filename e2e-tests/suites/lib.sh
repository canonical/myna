# shellcheck shell=bash
# Helpers every suite gets by sourcing "$SUITE_LIB" (run-suite.sh exports
# VM and REL; a suite runs on the host and reaches the VM through lxc).
#
#   shot [shot.sh options] [steps]   drive the app (tools/shot.sh)
#   on "command"                     run in the VM as the autologin user
#   assert_on "description" "cmd"    remote assertion, tallied
#   suite_status                     exit 1 if any assertion failed

# shellcheck source=e2e-tests/vm/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/../vm/lib.sh"

FAILED=0

on() { on_vm "$VM" "$1"; }

shot() { on "myna-shot/shot.sh $(printf '%q ' "$@")"; }

assert_on() {
    if on "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}

suite_status() {
    [ $FAILED -eq 0 ] || { echo "suite had failures" >&2; exit 1; }
}

# The accessible role GTK reports for plain buttons differs by toolkit
# (noble's 4.14 says "push button", newer say "button").
BTN=button
[ "$REL" != noble ] || BTN="push button"
export BTN
