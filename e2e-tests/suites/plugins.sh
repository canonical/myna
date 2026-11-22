#!/usr/bin/env bash
# snapshot: dictation
# requires: E2E_PLUGIN_BLOBS
# A text plugin from install to a typed dictation and back out, through the
# real Myna Settings (AT-SPI), the real daemon and the real snapd, with the
# fake speech backend and a virtual microphone: the Add step page offers the
# English to French translation, it is added with Tidy in stage order, a
# dictation types French, Remove takes the model away, the step says what it
# needs while the dictation types what was heard, and a reinstall heals it.
#
# E2E_PLUGIN_BLOBS is a directory with the snaps and components to sideload:
# myna-text_*.snap, myna-text+model-disfluency-en.comp, myna-translation_*.snap
# and myna-translation+model-translate-en-fr.comp (text-snap/ and
# translation-snap/ after `snapcraft pack`).
#
# Install mode. Snapd refuses a REST component install for a snap it does not
# know in the store, and neither text snap is registered yet
# (plugin-store-enablement), so Settings' Install cannot succeed on a
# sideloaded snap. E2E_PLUGIN_INSTALL picks the path, `auto` (the default)
# by asking the store whether it knows myna-translation:
#
#   sideload  STAND-IN. The suite presses the real Install, asserts that it
#             fails the way snapd says it must and changes nothing, then
#             installs the components with `snap install --dangerous` and
#             restarts the daemon (a component change empties the running
#             daemon's backend mounts, snapd-component-change-drops-backend-mounts).
#   store     the snaps come from the store and the components only through
#             Settings' Install, with no restart. NOT RUN YET: nothing in the
#             store can drive it today.
#
# Removal is real in both modes: snapd removes a sideloaded snap's component.
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
FR="Translate English to Français"
FR_MODEL="myna-translation+model-translate-en-fr"

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
# dictate: one full dictation into the plain field, waiting for the daemon.
dictate() {
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict dictation_toggle
    assert_dict "recording" 'wait_until 10 dictation_state_is recording'
    dict dictation_toggle
    assert_dict "idle after the stop" 'wait_until 20 dictation_state_is idle'
}
now() { on 'date +%s.%N'; }

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

MODE=${E2E_PLUGIN_INSTALL:-auto}
if [ "$MODE" = auto ]; then
    if on 'snap info myna-translation >/dev/null 2>&1'; then MODE=store; else MODE=sideload; fi
fi
echo "plugin install mode: $MODE"
[ "$MODE" = store ] && echo "NOTE: store mode has never run; nothing in the store can drive it yet"

echo "-- 1. the rig: a fake speech backend, a virtual microphone, the text snaps without models"
assert_on "precondition: the speech backend is the fake" \
    'snap connections myna | grep -q "myna:backend.*myna-fake-backend:provider"'
assert_dict "the virtual microphone is made the default source" 'mic_make_default'
dict 'mic_start myna-shot/speech.wav'
assert_dict "the virtual keyboard is up" 'keys_start'
dict 'key escape'
assert_dict "the daemon logs at the debug tier" 'daemon_debug on'
assert_dict "the field app is up" 'field_start'
assert_dict "the field is focused" 'field_focus plain'
if [ "$MODE" = store ]; then
    assert_on "the text snaps come from the store, connected" \
        'sudo snap install --edge myna-text myna-translation && sudo snap connect myna:backend myna-text:provider && sudo snap connect myna:backend myna-translation:provider'
else
    assert_dict "myna-text is installed without models and connected" "seed_text_snap myna-text $TEXT_SNAP"
    assert_dict "myna-translation is installed without models and connected" "seed_text_snap myna-translation $TRANS_SNAP"
fi
assert_dict "the daemon is restarted to see the shares" 'daemon_restart'
assert_dict "no text model is bound yet" \
    '! status_has "model .*text\.translate" && ! status_has "model .*text\.cleanup"'
assert_dict "the translation model is not installed" \
    "! component_installed myna-translation model-translate-en-fr"
assert_dict "the dictation language is English" 'snap run myna.config set language en'
dict 'daemon_pid > myna-shot/daemon.pid'
evidence 1

T0=$(now)
if [ "$MODE" = store ]; then
    echo "-- 2. Settings installs the translation model (store)"
    shot --polkit allow waitfor:General tab:Processing "waitfor:Add step" "click:Add step" \
        "waitfor:$FR" "click:Install $FR@$BTN" "waitfor:Add $FR@$BTN" \
        shot:shots/plugins/02-installed.png \
        || { echo "install run failed" >&2; FAILED=1; }
    T_INSTALL=$(now)
    assert_dict "the Install asked for one authorization" "[ \"\$(polkit_grants $T0)\" = 1 ]"
    assert_on "snapd has the translation model" \
        "snap components myna-translation | grep -q 'model-translate-en-fr.*installed'"
    assert_dict "Settings did not restart the daemon" '[ "$(daemon_pid)" = "$(cat myna-shot/daemon.pid)" ]'
else
    echo "-- 2. STAND-IN: Settings' Install is refused for a sideloaded snap, so the model is sideloaded"
    shot --polkit allow waitfor:General tab:Processing "waitfor:Add step" "click:Add step" \
        "waitfor:$FR" "waitfor:Install $FR@$BTN" shot:shots/plugins/02-add-step-install.png \
        "click:Install $FR@$BTN" "waitfor:Installing the model failed" shot:shots/plugins/02-install-refused.png \
        || { echo "refused install run failed" >&2; FAILED=1; }
    assert_dict "the refused install changed nothing" \
        "! component_installed myna-translation model-translate-en-fr"
    # shellcheck disable=SC2016 # expands in the VM
    assert_dict "the refused Install asked for one authorization, granted" \
        "[ \"\$(polkit_grants $T0)\" = 1 ]"
    assert_dict "the translation model is sideloaded (stand-in for Install)" \
        "sideload \$BLOBS/$FR_MODEL.comp"
    T_INSTALL=$(now)
    assert_dict "the disfluency model is sideloaded (stand-in for Install)" \
        "sideload \$BLOBS/myna-text+model-disfluency-en.comp"
    assert_dict "the daemon is restarted for the mounts (stand-in for snapd's fix)" 'daemon_restart'
fi
assert_dict "the daemon binds the translation model" \
    'wait_until 30 status_has "model .*myna-translation/.*text\.translate"'
echo "timing: install finished to translation bound in $(echo "$(now) - $T_INSTALL" | bc) s ($MODE)"
assert_dict "the daemon binds the disfluency model" \
    'wait_until 30 status_has "model .*myna-text/.*text\.cleanup"'
evidence 3

echo "-- 4. Translate is added before Tidy, and the steps are stored in stage order"
shot waitfor:General tab:Processing "waitfor:Add step" "click:Add step" \
    "click:Add $FR@$BTN" "waitfor:Tidy up speech@switch" \
    "$(poll "grep -q \"'translate.to': 'fr'\" $STORE")" \
    || { echo "add translate run failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "waitfor:Add step" "click:Add step" \
    "click:Add Tidy up speech@$BTN" "waitfor:Tidy up speech@switch" \
    "$(poll "grep -q \"processing-steps=\\['tidy', 'translate'\\]\" $STORE")" \
    shot:shots/plugins/04-steps.png \
    || { echo "add tidy run failed" >&2; FAILED=1; }
assert_on "stage order: Tidy comes before Translate whatever order they were added in" \
    "grep -q \"processing-steps=\\['tidy', 'translate'\\]\" $STORE"
assert_dict "the daemon reads both steps as applicable" \
    'wait_until 20 sh -c "snap run myna --status 2>&1 | grep -q \"step .*tidy applicable\" && snap run myna --status 2>&1 | grep -q \"step .*translate applicable\""'
shot waitfor:General tab:Processing "waitfor:Chinese script@switch" \
    "waitfor:Applies when the text is Chinese" shot:shots/plugins/04-script-dimmed.png \
    || { echo "script dim run failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "waitfor:Past dictations" "click:Past dictations" \
    "waitfor:Keep a record of what was changed@switch" \
    "click:Keep a record of what was changed@switch" \
    "$(poll "grep -q \"processing-history=true\" $STORE")" \
    || { echo "record switch run failed" >&2; FAILED=1; }

echo "-- 5. a dictation types French; a provider Myna does not curate is ignored"
assert_dict "a text provider outside the allow-list is installed and connected" 'rogue_install'
assert_dict "the daemon is restarted to see it" 'daemon_restart'
dict journal_mark
dict 'daemon_pid > myna-shot/daemon.pid'
dictate
assert_dict "French is in the field, not the English" \
    "field_get plain | grep -qi '$FRENCH_WORDS' && ! field_is plain '$FULL'"
assert_dict "the daemon did not restart" '[ "$(daemon_pid)" = "$(cat myna-shot/daemon.pid)" ]'
assert_dict "the rogue's cleanup model is not bound" '! status_has "myna-rogue-text/"'
assert_dict "the daemon logged the ignored provider by name" \
    "journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'myna-rogue-text: not a curated text provider'"
assert_dict "the daemon compiled Tidy before Translate" 'status_has "steps .*-> tidy, translate=fr"'
evidence 5

echo "-- 6. Past dictations lists the result"
shot waitfor:General tab:Processing "waitfor:Past dictations" "click:Past dictations" \
    "waitfor:$FULL" "waitfor:renard" shot:shots/plugins/06-past-dictations.png \
    || { echo "past dictations run failed" >&2; FAILED=1; }

echo "-- 7. Remove takes the model away: one authorization, no daemon restart, the step says what it needs"
T0=$(now)
dict 'daemon_pid > myna-shot/daemon.pid'
shot --polkit allow waitfor:General tab:Processing "waitfor:Add step" "click:Add step" \
    "click:$FR" "click:Remove $FR@$BTN" \
    "$(poll "! snap components myna-translation | grep -q 'model-translate-en-fr.*installed'")" \
    || { echo "remove run failed" >&2; FAILED=1; }
assert_dict "the translation model is gone" "! component_installed myna-translation model-translate-en-fr"
assert_dict "the Remove asked for one authorization" "[ \"\$(polkit_grants $T0)\" = 1 ]"
assert_dict "Settings did not restart the daemon" '[ "$(daemon_pid)" = "$(cat myna-shot/daemon.pid)" ]'
# A component change empties the daemon's mounts (snapd bug): the restart
# below is the stand-in for that, and without it the chain stays unbound.
assert_dict "the daemon is restarted for the mounts (stand-in for snapd's fix)" 'daemon_restart'
assert_dict "the daemon names what the chain lacks" \
    'wait_until 30 status_has "missing .*text\.translate .*to=fr"'
assert_dict "the step is enabled but unavailable" 'status_has "translate unavailable"'
shot waitfor:General tab:Processing "waitfor:Install the Français model" \
    shot:shots/plugins/07-needs-model.png \
    || { echo "needs-model run failed" >&2; FAILED=1; }
dictate
assert_dict "the English the backend heard is in the field (fail open)" "wait_until 10 field_is plain '$FULL'"
assert_dict "the daemon logged the skipped step without the text" \
    "journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'text.translate skipped'"
evidence 7

echo "-- 8. reinstalling heals the chain"
if [ "$MODE" = store ]; then
    T_INSTALL=$(now)
    shot --polkit allow waitfor:General tab:Processing "click:Install the Français model" \
        "gone:Install the Français model" \
        || { echo "reinstall run failed" >&2; FAILED=1; }
else
    assert_dict "the translation model is sideloaded again (stand-in for Install)" \
        "sideload \$BLOBS/$FR_MODEL.comp"
    T_INSTALL=$(now)
    assert_dict "the daemon is restarted for the mounts (stand-in for snapd's fix)" 'daemon_restart'
fi
assert_dict "the chain is ready again" 'wait_until 30 status_has "step .*translate applicable"'
echo "timing: reinstall finished to translation bound in $(echo "$(now) - $T_INSTALL" | bc) s ($MODE)"
dictate
assert_dict "French is typed again" "field_get plain | grep -qi '$FRENCH_WORDS'"
evidence 8

echo "-- 9. the steps are switched off and the rig is put back"
shot waitfor:General tab:Processing "waitfor:Tidy up speech@switch" \
    "click:Tidy up speech@switch" "click:Translate@switch" \
    "$(poll "! grep -q processing-steps $STORE || grep -q \"processing-steps=\\[\\]\" $STORE")" \
    || { echo "reset run failed" >&2; FAILED=1; }
dict 'snap run myna.config set language ""; mic_stop' >/dev/null 2>&1
suite_status
