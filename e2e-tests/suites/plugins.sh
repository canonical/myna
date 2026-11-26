#!/usr/bin/env bash
# snapshot: ready
# tags: plugins, text-model
# A text plugin from install to a typed dictation and back out, through the
# real Myna Settings (AT-SPI), the real daemon, the real snapd and the store,
# with the fake speech backend and a virtual microphone:
#
#   1. the rig: both text snaps from the store, connected, no model yet
#   2. Get more steps → Translate → Français makes and uses a chain and
#      installs the model through Settings (one polkit grant); the daemon
#      restarts itself once idle (the bridge for snapd LP#2175592, a
#      component change may empty its backend mounts) and binds it
#   3. Tidy is installed and added from the catalog: it lands before
#      Translate (stage order); Chinese script says why it does not apply
#   4. a dictation types French; a provider Myna does not curate is ignored
#   5. Past dictations lists the result
#   6. Remove on the language page: one grant, the step stays and says what
#      it needs, and the dictation types what was heard (fail open)
#   7. Install from the chain's editor heals it: French again
#
# The chains suite covers building chains; this one the plugin's life: the
# store, the allow-list, failing open and healing.
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"

REPO=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
FULL="The quick brown fox jumps over the lazy dog."
# What the English to French model says of FULL, in the words that matter.
FRENCH_WORDS='renard.*chien'
LIB='. myna-shot/plugin-lib.sh'
# shellcheck disable=SC2016 # expands in the VM
STORE='$HOME/snap/myna/common/.config/glib-2.0/settings/keyfile'
CHAIN="New chain"
FR="English to Français"

dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}
evidence() {
    dict "cd myna-shot/out
        cp field.json \"field-plugins-$1.json\" 2>/dev/null
        daemon_status > \"status-plugins-$1.txt\" 2>&1
        journalctl --user -u snap.myna.myna.service --no-pager -o short-precise > daemon.log 2>&1
        true" >/dev/null 2>&1
    return 0
}
dictate() {
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict dictation_toggle
    assert_dict "recording" 'wait_until 10 dictation_state_is recording'
    dict dictation_toggle
    assert_dict "idle after the stop" 'wait_until 20 dictation_state_is idle'
}
now() { on 'date +%s.%N'; }
# shellcheck disable=SC2016 # expands in the VM
slowpoll() { printf 'sh:for _ in $(seq 1500); do { %s; } >/dev/null 2>&1 && exit 0; sleep 0.2; done; exit 1' "$1"; }
OFFERS='gdbus call --session --dest com.canonical.Myna.Dictation --object-path /com/canonical/Myna/Dictation --method org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation InstalledSteps'
installed() { printf "snap components %s | grep -q '%s *installed'" "$1" "$1+$2"; }
# Each component change may empty the running daemon's backend mounts (snapd
# LP#2175592, about half of them): the daemon then restarts itself once idle
# (myna_desktop::restart). Either way it must end up bound or degraded as
# asserted below; the suite never restarts it.

echo "-- 1. the rig: both text snaps from the store, connected, no model yet"
on 'mkdir -p myna-shot/out'
lxc file push --uid 1000 --gid 1000 "$REPO/tests/spread/adapter-smoke/fixture.wav" "$VM/home/ubuntu/myna-shot/speech.wav"
assert_on "precondition: the speech backend is the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_dict "the virtual microphone is made the default source" 'mic_make_default'
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
dict 'key escape'
assert_dict "the daemon logs at the debug tier" 'daemon_debug on'
assert_dict "the field app is up" 'field_start'
assert_dict "the field is focused" 'field_focus plain'
assert_on "the text snaps come from the store" \
    '{ sudo snap install --edge myna-text && sudo snap install --edge myna-translation; } > myna-shot/out/text-install.txt 2>&1'
# shellcheck disable=SC2016 # expands in the VM
assert_on "both are connected to Myna" \
    'for s in myna-text myna-translation; do snap connections myna | grep -q "myna:backend.*$s:provider" || sudo snap connect "myna:backend" "$s:provider" || exit 1; done'
assert_dict "the daemon is restarted to see the shares" 'daemon_restart'
assert_dict "no chain, English dictation" \
    "snap run myna.config set chains '[]' && snap run myna.config set active-chain '' && snap run myna.config set language en"
assert_dict "the translation model is not installed" \
    "! component_installed myna-translation model-translate-en-fr"
evidence 1

echo "-- 2. Get more steps → Translate → Français: a chain made and used, the model installed"
T0=$(now)
dict journal_mark
shot --polkit allow waitfor:General tab:Processing "click:Get more steps" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:fran" "gone:Español" \
    "click:Français" "$(slowpoll "$(installed myna-translation model-translate-en-fr)")" \
    "$(slowpoll "$OFFERS | grep -q en-fr")" \
    shot:shots/plugins/02-installed.png \
    || { echo "install run failed" >&2; FAILED=1; }
assert_on "the chain is made, holds Translate to French and is in use" \
    "grep -qF \"'$CHAIN', [('translate', true, {'to': 'fr'})]\" $STORE && ! grep -q \"active-chain=''\" $STORE"
assert_dict "the Install asked for one authorization" "[ \"\$(polkit_grants $T0)\" = 1 ]"
assert_dict "the daemon binds the translation model" \
    'wait_until 60 status_has "model .*myna-translation/.*text\.translate"'
evidence 2

echo "-- 3. Tidy is installed and added: it lands before Translate"
dict journal_mark
shot --polkit allow waitfor:General tab:Processing "click:Get more steps" \
    "waitfor:Applies when the text is Chinese" shot:shots/plugins/03-script-not-applicable.png \
    "click:Tidy up speech" "click:Install Tidy up speech@$BTN" \
    "$(slowpoll "$(installed myna-text model-disfluency-en)")" \
    "$(slowpoll "$OFFERS | grep -q disfluency")" \
    "click:Add Tidy up speech@$BTN" \
    "$(poll "grep -qF \"'$CHAIN', [('tidy', true, {}), ('translate', true\" $STORE")" \
    shot:shots/plugins/03-steps.png \
    || { echo "add tidy run failed" >&2; FAILED=1; }
assert_on "stage order: Tidy comes before Translate whatever order they were added in" \
    "grep -qF \"'$CHAIN', [('tidy', true, {}), ('translate', true\" $STORE"
assert_dict "the daemon binds the disfluency model" \
    'wait_until 60 status_has "model .*myna-text/.*text\.cleanup"'
shot waitfor:General tab:Processing "waitfor:Past dictations" "click:Past dictations" \
    "waitfor:Keep a record of what was changed" \
    "click:Keep a record of what was changed" \
    "$(poll "grep -q \"processing-history=true\" $STORE")" \
    || { echo "record switch run failed" >&2; FAILED=1; }
evidence 3

echo "-- 4. a dictation types French; a provider Myna does not curate is ignored"
assert_dict "a text provider outside the allow-list is installed and connected" 'rogue_install'
assert_dict "the daemon is restarted to see it" 'daemon_restart'
dict journal_mark
dictate
assert_dict "French is in the field, not the English" \
    "field_get plain | grep -qi '$FRENCH_WORDS' && ! field_is plain '$FULL'"
assert_dict "the rogue's cleanup model is not bound" '! status_has "myna-rogue-text/"'
assert_dict "the daemon logged the ignored provider by name" \
    "journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'myna-rogue-text: not a curated text provider'"
assert_dict "the daemon compiled Tidy before Translate" 'status_has "steps .*-> tidy, translate=fr"'
evidence 4

echo "-- 5. Past dictations lists the result"
shot waitfor:General tab:Processing "waitfor:Past dictations" "click:Past dictations" \
    "waitfor:$FULL" "waitfor:renard" shot:shots/plugins/05-past-dictations.png \
    || { echo "past dictations run failed" >&2; FAILED=1; }

echo "-- 6. Remove takes the model away; the step says what it needs and the dictation fails open"
T0=$(now)
dict journal_mark
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "click:Translate" "waitfor:Remove $FR" "click:Remove $FR@$BTN" \
    "$(slowpoll "! snap components myna-translation | grep -q 'model-translate-en-fr *installed'")" \
    "$(slowpoll "! $OFFERS | grep -q en-fr")" \
    || { echo "remove run failed" >&2; FAILED=1; }
assert_dict "the translation model is gone" "! component_installed myna-translation model-translate-en-fr"
assert_dict "the Remove asked for one authorization" "[ \"\$(polkit_grants $T0)\" = 1 ]"
assert_dict "the daemon names what the chain lacks" \
    'wait_until 60 status_has "missing .*text\.translate .*to=fr"'
assert_on "the chain keeps its step" "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Install" shot:shots/plugins/06-needs-model.png \
    || { echo "needs-model run failed" >&2; FAILED=1; }
dictate
assert_dict "the English the backend heard is in the field (fail open)" "wait_until 10 field_is plain '$FULL'"
assert_dict "the daemon logged the skipped step without the text" \
    "journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'text.translate skipped'"
evidence 6

echo "-- 7. Install from the chain's editor heals it"
dict journal_mark
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Install@$BTN" \
    "$(slowpoll "$(installed myna-translation model-translate-en-fr)")" \
    "$(slowpoll "$OFFERS | grep -q en-fr")" \
    || { echo "reinstall run failed" >&2; FAILED=1; }
assert_dict "the chain is ready again" 'wait_until 60 status_has "step .*translate applicable"'
dictate
assert_dict "French is typed again" "field_get plain | grep -qi '$FRENCH_WORDS'"
evidence 7

dict "snap run myna.config set chains '[]'; snap run myna.config set active-chain ''; snap run myna.config set language ''; mic_stop" >/dev/null 2>&1
suite_status
