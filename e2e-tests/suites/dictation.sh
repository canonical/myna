#!/usr/bin/env bash
# snapshot: dictation
# Dictation end to end on the real desktop session: activation -> a speech
# clip played through a virtual microphone -> the fake backend (a scripted
# transcript: "The quick brown fox" while recording, "jumps over the lazy dog."
# at the end) -> IBus -> a text field (tools/field-app.py: Qt on Plasma, GTK4
# elsewhere), asserted from the field's own report and the daemon's state,
# never from pixels.
#
#   1. a toggle dictates into a plain field
#   2. a password field receives nothing and the press is refused
#   3. focus moved away mid-utterance: the rest is dropped, nothing lands in
#      the field that took the focus (on Plasma a field in another window:
#      see tools/field-app.py)
#   4. the bound key (a virtual keyboard, tools/uinput-keys.py, presses it on
#      the real seat) starts and stops a dictation
#   5. the key held 2 s (past the focus wait and the 1 s blip grace, deep
#      into key repeat) still starts one: on X11 its grab keeps focus off the
#      field until release, and its repeats keep the daemon waiting for it
#   6. the key held 1.5 s to stop toggles once and the tail still lands
#   7. the first dictation after ibus-daemon restarts, by key, reaches the
#      field: the daemon focuses the engine before it can name the field
#   8. (Xubuntu, Plasma) the pill through a dictation, photographed on the
#      real desktop: listening, finishing, the focus-lost notice, the
#      secure-field error. The HUD is hosted by myna-hud-host, as the deb's
#      autostart entry does (an X11 window on Xfce, a layer surface on
#      KWin); GNOME's shell extension is not in the VM. The case waits for
#      the HUD to register, then asserts the pill's window state, that the
#      field kept focus and that the daemon sent no toast.
#   9. (Xubuntu, Plasma) the daemon restarts and a dictation starts at once,
#      with no wait for the HUD: the host's registration stands in for the
#      pill that is on its way, so no toast, and the pill shows while still
#      listening.
#  10. the backend streaming: its hypothesis goes out as preedit and the whole
#      transcript, never the hypothesis, lands. On GNOME the preedit reaches
#      the Shell's own IBus client, which libibus 1.5.33-1.5.34 crash with a
#      non-empty attribute list (#18): the Shell logs no IBusAttrList double
#      free through two GJS collections and is still the same process.
#  11. (Xubuntu, Plasma) logging out ends the HUD host and its HUD.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
FIRST="The quick brown fox"
FULL="The quick brown fox jumps over the lazy dog."
LIB='. myna-shot/dictation-lib.sh'

# dict "command": run a dictation-lib command in the VM.
dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}
# What the field and the daemon looked like, for a human, whatever happened.
evidence() {
    dict "cd myna-shot/out
        cp field.json \"field-$1.json\" 2>/dev/null
        journalctl --user -u snap.myna.myna.service --no-pager -o short-precise > daemon.log 2>&1
        { ibus address; ibus engine; } > ibus.txt 2>&1
        true" >/dev/null 2>&1
    return 0
}

on 'mkdir -p myna-shot/out'
lxc file push --uid 1000 --gid 1000 "$REPO/tests/spread/adapter-smoke/fixture.wav" "$VM/home/ubuntu/myna-shot/speech.wav"

assert_on "precondition: the backend is the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_on "precondition: the daemon is idle" \
    "$LIB; wait_until 20 dictation_state_is idle"
assert_on "precondition: PipeWire runs realtime" "$LIB; pipewire_realtime"
assert_on "the virtual microphone is made the default source" \
    "$LIB; mic_make_default"
assert_on "precondition: the virtual microphone is the default source" \
    "$LIB; default_source_is myna-e2e-mic"
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
# GNOME starts in the Overview, where a new window is not focused.
dict 'key escape'
assert_dict "the field app is up" 'field_start'
assert_dict "the field is focused and the window active" 'field_focus plain'

echo "-- 1. a toggle dictates into a plain field"
assert_dict "the field starts empty" 'field_clear'
dict dictation_toggle
assert_dict "recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the microphone's audio reaches the daemon" 'wait_until 10 dictation_hears_audio'
assert_dict "the first segment lands while recording" "wait_until 10 field_is plain '$FIRST'"
dict dictation_toggle
assert_dict "idle after the stop" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
assert_dict "no other field changed" 'field_is other "" && field_is secret ""'
evidence 1

echo "-- 2. a password field receives nothing"
assert_dict "the field is cleared" 'field_clear'
assert_dict "the password field is focused" 'field_focus secret'
dict dictation_toggle
assert_dict "the press is refused (error state)" 'wait_until 10 dictation_state_is error'
assert_dict "the daemon says why" \
    "journalctl --user -u snap.myna.myna.service --no-pager -n 20 | grep -q 'focused field is secure'"
assert_dict "nothing reached any field" \
    'sleep 2; field_is plain "" && field_is other "" && field_is secret ""'
evidence 2

echo "-- 3. focus moved away mid-utterance"
assert_dict "the field is cleared" 'field_clear'
assert_dict "the plain field is focused" 'field_focus plain'
dict dictation_toggle
assert_dict "the first segment lands while recording" "wait_until 10 field_is plain '$FIRST'"
assert_dict "focus moves to the other field" 'field_focus other'
assert_dict "the utterance ends on the focus loss" 'wait_until 10 dictation_state_is idle'
assert_dict "the rest of the transcript was dropped" \
    "sleep 2; field_is plain '$FIRST' && field_is other ''"
evidence 3

echo "-- 4. the bound key starts and stops a dictation"
dict 'session_env; bind_toggle_key "<Super>j"'
assert_dict "the field is cleared" 'field_clear'
assert_dict "the plain field is focused" 'field_focus plain'
dict 'key super+j'
assert_dict "the key starts recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
dict 'key super+j'
assert_dict "the key stops it" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
evidence 4

echo "-- 5. the key held 2 s starts a dictation"
assert_dict "the field is cleared" 'field_clear'
# Refocused: GNOME Shell keeps a cleared field's old surrounding text until
# focus moves, so a dictation straight after case 4 would open with a space.
assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
dict 'key super+j@2000'
# The keyboard queues keys: let the hold end, or the stop below follows its
# last repeat faster than a person could press again.
dict 'sleep 2.5'
assert_dict "the held key starts recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
dict 'key super+j'
assert_dict "the key stops it" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
evidence 5

echo "-- 6. the key held 1.5 s toggles once"
assert_dict "the field is cleared" 'field_clear'
assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
dict journal_mark
dict dictation_toggle
assert_dict "recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
dict 'key super+j@1500'
assert_dict "the held key stops it" 'wait_until 10 dictation_state_is idle'
assert_dict "and nothing starts again" 'sleep 3; dictation_state_is idle'
assert_dict "the whole transcript is in the field" "field_is plain '$FULL'"
# shellcheck disable=SC2016 # expands in the VM
assert_dict "one start and one stop, no more" \
    '[ "$(journal_count "ctrl: press")" = 1 ] && [ "$(journal_count "ctrl: release")" = 1 ]'
evidence 6

echo "-- 7. the first dictation by key after ibus-daemon restarts"
assert_dict "ibus-daemon restarted" 'ibus_restart'
assert_dict "the field is cleared" 'field_clear'
assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
dict 'key super+j'
assert_dict "the key starts recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
dict 'key super+j'
assert_dict "the key stops it" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
evidence 7

HOSTED=''
case $DESKTOP in xubuntu) HOSTED=x11 ;; kde) HOSTED=layer-shell ;; esac
if [ -n "$HOSTED" ]; then
    echo "-- 8. the pill through a dictation, on the real desktop"
    dict journal_mark
    assert_dict "the HUD host runs the HUD" 'hud_host_start'
    assert_dict "as the session's kind of host ($HOSTED)" "hud_host_kind_is $HOSTED"
    assert_dict "the toast watch is up" 'notify_watch_start'
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict active_window_save
    assert_dict "the HUD is registered with the daemon" 'wait_until 30 hud_registered'
    dict dictation_toggle
    assert_dict "recording" 'wait_until 10 dictation_state_is recording'
    assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
    assert_dict "the pill shows while listening" 'wait_until 10 pill_shows'
    assert_dict "the pill is a no-focus notification window, bottom centre of the work area" 'pill_placed'
    assert_dict "the field app is still the active window" 'active_window_kept'
    assert_dict "the daemon did not fall back to a toast" 'no_myna_toast'
    dict 'checkpoint 01-listening'
    dict backend_freeze
    dict dictation_toggle
    assert_dict "finalizing while the backend is held" 'wait_until 10 dictation_state_is finalizing'
    assert_dict "the pill shows while finishing" 'wait_until 10 pill_shows'
    dict 'checkpoint 02-finishing'
    dict backend_thaw
    assert_dict "idle once the backend answers" 'wait_until 10 dictation_state_is idle'
    assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
    dict 'checkpoint 03-dictated'

    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict dictation_toggle
    assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
    assert_dict "focus moves to the other field" 'field_focus other'
    assert_dict "the daemon tells the user focus was lost (notice state)" 'wait_until 10 dictation_state_is notice'
    assert_dict "the pill shows the notice" 'wait_until 10 pill_shows'
    dict 'checkpoint 04-notice'
    assert_dict "idle once the notice has passed" 'wait_until 15 dictation_state_is idle'

    assert_dict "the password field is focused" 'field_focus secret'
    dict dictation_toggle
    assert_dict "the press is refused (error state)" 'wait_until 10 dictation_state_is error'
    assert_dict "the pill shows the error" 'wait_until 10 pill_shows'
    dict 'checkpoint 05-error'
    assert_dict "the HUD logged no GTK critical" 'hud_log_clean'
    evidence 8

    echo "-- 9. a dictation the moment the daemon is back, before its HUD is"
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict notify_mark
    dict 'daemon_restart'
    # No wait for the HUD: it needs seconds to register, and the host has
    # already said a pill is coming.
    dict dictation_toggle
    assert_dict "recording" 'wait_until 15 dictation_state_is recording'
    assert_dict "the daemon did not fall back to a toast" 'no_myna_toast_since_mark'
    assert_dict "the pill shows while still listening" 'wait_until 30 pill_while_listening'
    assert_dict "still no toast, now the pill is up" 'no_myna_toast_since_mark'
    dict 'checkpoint 06-listening-after-restart'
    dict dictation_toggle
    assert_dict "idle after the stop" 'wait_until 10 dictation_state_is idle'
    assert_dict "the whole transcript is in the field" "wait_until 10 field_is plain '$FULL'"
    assert_dict "the HUD logged no GTK critical" 'hud_log_clean'
    evidence 9
fi

echo "-- 10. a streaming backend's hypothesis as preedit"
assert_dict "the daemon logs at the debug tier" 'daemon_debug on'
assert_dict "the fake backend streams" 'fake_streaming true'
assert_dict "the field is cleared" 'field_clear'
assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
dict 'shell_pid > myna-shot/shell.pid; journal_mark'
dict dictation_toggle
assert_dict "recording" 'wait_until 10 dictation_state_is recording'
assert_dict "the first segment lands" "wait_until 10 field_is plain '$FIRST'"
dict dictation_toggle
assert_dict "idle after the stop" 'wait_until 10 dictation_state_is idle'
assert_dict "the whole transcript, not the hypothesis, is in the field" \
    "wait_until 10 field_is plain '$FULL'"
# shellcheck disable=SC2016 # expands in the VM
assert_dict "the hypothesis went out as preedit" '[ "$(journal_count "inject: preedit(")" -ge 1 ]'
if [ "$DESKTOP" = gnome ]; then
    # shellcheck disable=SC2016 # expands in the VM
    assert_dict "GNOME Shell freed no IBusAttrList twice (#18)" \
        'sleep 25; [ "$(shell_attr_list_criticals)" = 0 ]'
    # shellcheck disable=SC2016 # expands in the VM
    assert_dict "GNOME Shell is still the same process" \
        '[ "$(shell_pid)" = "$(cat myna-shot/shell.pid)" ]'
fi
evidence 10
dict 'fake_streaming false; daemon_debug off' >/dev/null 2>&1
# shellcheck disable=SC2016 # expands in the VM
assert_dict "no dictation lost audio" '[ "$(audio_lost_count)" = 0 ]'

dict 'mic_stop' >/dev/null 2>&1
if [ -n "$HOSTED" ]; then
    echo "-- 11. the HUD host ends with the session"
    dict session_logout
    assert_dict "the host and the HUD are gone" 'wait_until 20 hud_host_ended'
    assert_dict "the host saw the session end" "grep -q 'the session has ended' myna-shot/out/hud-host.log"
fi
suite_status
