#!/usr/bin/env bash
# snapshot: dictation
# requires: E2E_PLUGIN_BLOBS
# What the user waits for after they stop, and what they get when the text
# model is broken, on the real GNOME session: the real daemon, IBus and
# snapd, the real myna-text and myna-translation snaps with their models,
# the fake speech backend (so the wait that is left is Myna's own) and a
# virtual microphone.
#
# - The first dictation after the daemon starts does not stall on a cold bind
#   (Tidy and Tidy then French, under a second).
# - Warm, the wait past the speech model's own tail (dead_air_ms minus
#   asr_tail_ms in the record of past dictations) has a p95 under 100 ms for
#   Tidy and under 1000 ms for Tidy then French.
# - A stopped myna-text, a hung one, and one that answers with a chat reply,
#   control characters, a deletion of nearly every word or nothing all type
#   the words that were heard, every time, and the daemon's journal never
#   holds a word of them.
#
# E2E_PLUGIN_BLOBS is the directory the plugins suite takes. E2E_DEADAIR_N is
# the number of warm dictations per chain (default 30).
# shellcheck disable=SC2016,SC2034 # commands expand in the VM; FAILED is lib.sh's
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
N=${E2E_DEADAIR_N:-30}
FULL="The quick brown fox jumps over the lazy dog."
LIB='. myna-shot/plugin-lib.sh'
FR_MODEL="myna-translation+model-translate-en-fr"
UNIT=snap.myna.myna.service

dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}
# stat_dict: the command's STAT line is the measurement, whatever it decides.
stat_dict() { # description command
    local out
    out=$(dict "$2" 2>&1); local rc=$?
    echo "$out" | grep -E '^STAT|did not type' || true
    if [ $rc = 0 ]; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}

for f in myna-text_*.snap myna-text+model-disfluency-en.comp \
    myna-translation_*.snap "$FR_MODEL.comp"; do
    compgen -G "$E2E_PLUGIN_BLOBS/$f" >/dev/null \
        || { echo "E2E_PLUGIN_BLOBS lacks $f" >&2; exit 1; }
done
on 'mkdir -p myna-shot/out myna-shot/blobs'
for f in "$E2E_PLUGIN_BLOBS"/myna-text* "$E2E_PLUGIN_BLOBS"/myna-translation*; do
    lxc file push --uid 1000 --gid 1000 "$f" "$VM/home/ubuntu/myna-shot/blobs/"
done
lxc file push --uid 1000 --gid 1000 "$REPO/tests/spread/adapter-smoke/fixture.wav" "$VM/home/ubuntu/myna-shot/speech.wav"
TEXT_SNAP=$(basename "$(compgen -G "$E2E_PLUGIN_BLOBS/myna-text_*.snap" | head -1)")
TRANS_SNAP=$(basename "$(compgen -G "$E2E_PLUGIN_BLOBS/myna-translation_*.snap" | head -1)")

echo "-- 1. the rig: the fake speech backend, a virtual microphone, both text snaps with their models"
assert_on "precondition: the speech backend is the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_dict "the virtual microphone is made the default source" 'mic_make_default'
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
dict 'key escape'
assert_dict "the field app is up" 'field_start'
assert_dict "myna-text is installed and connected" "seed_text_snap myna-text $TEXT_SNAP"
assert_dict "myna-translation is installed and connected" "seed_text_snap myna-translation $TRANS_SNAP"
assert_dict "the disfluency model is installed" "sideload \$BLOBS/myna-text+model-disfluency-en.comp"
assert_dict "the translation model is installed" "sideload \$BLOBS/$FR_MODEL.comp"
assert_dict "the dictation language is English" 'snap run myna.config set language en'
assert_dict "Tidy is the chain, the record is on" "chain_set \"['tidy']\" '{}'"
assert_dict "the daemon is restarted for the mounts and the settings" 'daemon_restart'
assert_dict "the daemon binds the disfluency model" \
    'wait_until 30 status_has "model .*myna-text/.*text\.cleanup"'
assert_dict "the field is focused" 'field_focus plain'
assert_dict "the fake backend's words are what the field should hold" \
    'history_clear; dictate_once && wait_until 5 field_is plain "'"$FULL"'"'

echo "-- 2. first dictation after the daemon starts, Tidy"
assert_dict "five daemon starts, each with a first dictation" 'firsts_collect 5'
stat_dict "Tidy: first dictation after start waits under 1000 ms" 'dead_air_firsts tidy 1000'

echo "-- 3. warm Tidy, $N dictations"
dict 'history_clear'
assert_dict "$N dictations type what was said" "dictate_many $N '$FULL'"
stat_dict "warm Tidy: p95 under 100 ms" 'dead_air_stats tidy 100 100'
dict 'cp "$HISTORY_FILE" myna-shot/out/history-tidy.jsonl'

echo "-- 4. Tidy then French: the first dictation after start, then $N warm"
assert_dict "Tidy then French is the chain" "chain_set \"['tidy', 'translate']\" \"{'translate.to': 'fr'}\""
assert_dict "the daemon is restarted" 'daemon_restart'
assert_dict "the daemon binds both models" \
    'wait_until 30 status_has "model .*myna-translation/.*text\.translate" && status_has "model .*myna-text/.*text\.cleanup"'
assert_dict "five daemon starts, each with a first dictation" 'firsts_collect 5'
stat_dict "Tidy then French: first dictation after start waits under 1000 ms" 'dead_air_firsts tidy-french 1000'
dict 'history_clear'
assert_dict "$N French dictations type French" "dictate_many_matching $N 'renard.*chien'"
stat_dict "warm Tidy then French: p95 under 1000 ms" 'dead_air_stats tidy-french 1000 1000'
dict 'cp "$HISTORY_FILE" myna-shot/out/history-tidy-french.jsonl'

echo "-- 5. a broken myna-text: the words that were heard are typed, every time"
assert_dict "Tidy is the chain" "chain_set \"['tidy']\" '{}'"
assert_dict "the daemon is restarted" 'daemon_restart'
dict journal_mark
# fault NAME: N dictations, each typing exactly what was heard.
fault() { # name description
    dict 'history_clear'
    assert_dict "$2: every dictation types what was heard" "dictate_many 6 '$FULL'"
    stat_dict "$2: the wait" 'dead_air_stats "'"$1"'" 5000 5000'
}
echo "-- 5a. stopped"
assert_on "myna-text is stopped" 'sudo snap stop myna-text'
fault stopped "a stopped myna-text"
echo "-- 5b. hung (SIGSTOP on its server)"
assert_on "myna-text is running again" 'sudo snap start myna-text'
assert_dict "the daemon binds it" 'daemon_restart; wait_until 30 status_has "model .*myna-text/.*text\.cleanup"'
assert_on "myna-text's server is stopped in its tracks" \
    'pid=$(systemctl show -P MainPID snap.myna-text.server.service); [ "${pid:-0}" -gt 1 ] && sudo kill -STOP "$pid"'
fault hung "a hung myna-text"
assert_on "myna-text's server is let go" \
    'sudo kill -CONT "$(systemctl show -P MainPID snap.myna-text.server.service)"'
echo "-- 5c. wrong answers"
assert_on "myna-text is stopped, a provider that passes for it takes its place" 'sudo snap stop myna-text'
assert_dict "the faulty provider is installed and connected" 'fault_install'
assert_dict "the daemon is restarted to see it" 'daemon_restart'
for mode in ok chatty control overdelete empty hang; do
    assert_on "the provider answers: $mode" "echo $mode | sudo tee /var/snap/myna-fault-text/common/mode"
    if [ "$mode" = ok ]; then
        dict 'history_clear; dictate_once'
        assert_dict "the faulty provider is the one the daemon bound" 'status_has "model .*myna-text/.*fault-cleanup"'
    fi
    fault "wrong-$mode" "a provider answering $mode"
done
echo "-- 5d. nothing said leaks into the journal"
assert_dict "the daemon's journal holds none of the dictated words" \
    "! journalctl --user -u $UNIT --no-pager --since \"@\$(cat \$JOURNAL_MARK)\" | grep -qiE 'quick brown|lazy dog|jumps over|renard'"
assert_dict "the system journal holds none of them from a myna unit" \
    "! sudo journalctl --no-pager --since \"@\$(cat \$JOURNAL_MARK)\" | grep -iE 'quick brown|lazy dog|jumps over' | grep -qiE 'myna'"

echo "-- 6. the rig is put back"
dict 'sudo snap remove myna-fault-text >/dev/null 2>&1; sudo snap start myna-text >/dev/null 2>&1
    snap run myna.config set language ""; mic_stop' >/dev/null 2>&1
suite_status
