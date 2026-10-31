#!/usr/bin/env bash
# snapshot: dictation
# Bench only, never merged: dictation case 5 (the key held 2 s starts a
# dictation) in a loop at the daemon's debug tier, to catch its rare "Focus
# lost" abort with the lease's focus changes logged.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

FIRST="The quick brown fox"
LIB='. myna-shot/dictation-lib.sh'
TRIES=${HELD_KEY_TRIES:-40}
dict() { on "$LIB; $1"; }
assert_dict() {
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}

on 'mkdir -p myna-shot/out'
lxc file push --uid 1000 --gid 1000 "$(git -C "$(dirname "$0")" rev-parse --show-toplevel)/tests/spread/adapter-smoke/fixture.wav" "$VM/home/ubuntu/myna-shot/speech.wav"
assert_on "precondition: the daemon is idle" "$LIB; wait_until 20 dictation_state_is idle"
assert_dict "the daemon logs at the debug tier" 'daemon_debug on'
assert_on "the virtual microphone is made the default source" "$LIB; mic_make_default"
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
dict 'key escape'
assert_dict "the field app is up" 'field_start'
assert_dict "the field is focused" 'field_focus plain'

lost=0
for i in $(seq "$TRIES"); do
    echo "-- try $i"
    dict 'field_clear; field_focus other && field_focus plain' >/dev/null 2>&1
    dict 'key super+j@2000'
    dict 'sleep 2.5'
    if dict 'wait_until 10 dictation_state_is recording' >/dev/null 2>&1; then
        echo "PASS: try $i recording"
        dict "wait_until 10 field_is plain '$FIRST'" >/dev/null 2>&1
        dict 'key super+j'
    else
        echo "FAIL: try $i did not start"; lost=$((lost + 1)); FAILED=1
    fi
    dict 'wait_until 15 dictation_state_is idle' >/dev/null 2>&1
done
echo "held-key: $lost of $TRIES tries did not start"
dict 'cd myna-shot/out; journalctl --user -u snap.myna.myna.service --no-pager -o short-precise > daemon.log 2>&1; true' >/dev/null 2>&1
suite_status
