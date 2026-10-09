# shellcheck shell=bash
# Helpers every suite gets by sourcing "$SUITE_LIB" (run-suite.sh exports
# VM and REL; a suite runs on the host and reaches the VM through lxc).
#
#   shot [shot.sh options] [steps]   drive the app (tools/shot.sh)
#   on "command"                     run in the VM as the autologin user
#   assert_on "description" "cmd"    remote assertion, tallied
#   assert_shortcut_bound            the dictation key is in the desktop's store
#   assert_input_method_up           the injector's input method is running
#   suite_status                     exit 1 if any assertion failed
#
# DESKTOP (gnome|xubuntu) is exported by run-suite.sh. Where the desktops
# legitimately differ, the difference lives here, not in the suites.

# shellcheck source=e2e-tests/vm/lib.sh
source "$(dirname "${BASH_SOURCE[0]}")/../vm/lib.sh"

FAILED=0

on() { on_vm "$VM" "$1"; }

DESKTOP=${DESKTOP:-$E2E_DESKTOP}

# The app runs on the real display of an X11 desktop, so its screenshots show
# the panel, theme and pill; GNOME's Wayland session cannot be photographed
# from inside, so there it runs under Xvfb.
SHOT_DISPLAY=xvfb
[ "$DESKTOP" != xubuntu ] || SHOT_DISPLAY=real

shot() { on "myna-shot/shot.sh --display $SHOT_DISPLAY $(printf '%q ' "$@")"; }

assert_on() {
    if on "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}

# assert_soon "description" "cmd": like assert_on, for state that settles
# after the UI has moved on (a daemon restarting); polls for 30 s.
assert_soon() {
    if on "for _ in \$(seq 150); do { $2; } >/dev/null 2>&1 && exit 0; sleep 0.2; done; exit 1"; then
        echo "PASS: $1"
    else
        echo "FAIL: $1"; FAILED=1
    fi
}

# poll "command": a shot.sh step that waits, up to 30 s, for a command run in
# the VM to succeed, so a flow waits on machine state rather than a sleep.
# shellcheck disable=SC2016 # the loop runs in the VM
poll() { printf 'sh:for _ in $(seq 150); do { %s; } >/dev/null 2>&1 && exit 0; sleep 0.2; done; exit 1' "$1"; }

# The step that waits for the cancel agent (shot --polkit cancel) to have
# dismissed a prompt: what follows is the app's reaction to the dismissal.
# shellcheck disable=SC2034 # used by the suites
PROMPT_DISMISSED=$(poll 'grep -q dismissing out/agent.log')

# The dictation key set up by onboarding, in the desktop's own shortcut store:
# a media-keys custom keybinding on GNOME, an xfsettingsd custom command on
# Xfce, where it is only live while /commands/custom/override is true.
# shellcheck disable=SC2016 # the commands expand in the VM
assert_shortcut_bound() {
    case $DESKTOP in
        gnome) assert_on "Super+J custom shortcut written (dconf)" \
            'dconf read /org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/binding | grep -q Super' ;;
        xubuntu) assert_on "Super+J custom shortcut written (xfconf)" \
            'xfconf-query -c xfce4-keyboard-shortcuts -lv | grep "^/commands/custom/<Super>" | grep -q com.canonical.Myna.Dictation \
            && [ "$(xfconf-query -c xfce4-keyboard-shortcuts -p /commands/custom/override)" = true ]' ;;
    esac
}

# The input method dictated text goes through: ibus-daemon on both desktops,
# and on Xfce also the session environment that makes toolkits use it.
# shellcheck disable=SC2016 # the commands expand in the VM
assert_input_method_up() {
    assert_on "ibus-daemon is running" 'pgrep -u "$USER" -x ibus-daemon >/dev/null'
    [ "$DESKTOP" != xubuntu ] || assert_on "the Xfce session selects IBus" \
        'systemctl --user show-environment | grep -qx "GTK_IM_MODULE=ibus"'
}

suite_status() {
    [ $FAILED -eq 0 ] || { echo "suite had failures" >&2; exit 1; }
}

# The accessible role GTK reports for plain buttons differs by toolkit
# (noble's 4.14 says "push button", newer say "button").
BTN=button
[ "$REL" != noble ] || BTN="push button"
export BTN
