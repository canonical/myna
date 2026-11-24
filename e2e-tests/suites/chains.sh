#!/usr/bin/env bash
# snapshot: chains
# Chains on the Processing tab, driven like a user: the real Myna Settings
# (xdotool clicks through tools/shot-driver.py, AT-SPI to find the widgets),
# the real daemon (the tree's snap, E2E_MYNA_SNAP), the real snapd and store,
# the fake speech backend and a virtual microphone. Every case asserts the
# keyfile and the daemon, never pixels; each state is a screenshot checkpoint
# (shots/chains/NN-name.png) for the contact sheet.
#
#   1. with no chain the page says so
#   2. New chain… makes a chain and opens its editor, empty
#   3. Add step… lists jobs, not models
#   4. Translate opens a searchable language page; choosing Français adds the
#      step and Settings installs the model from the store (one prompt)
#   5. the daemon restarts itself once to see the new share (the bridge for
#      snapd LP#2175592) and binds the translation
#   6. the chain switched on from its row: a dictation types French, and the
#      daemon's ChainStatus is ready; its editor offers Stop using, and at
#      360 px the target picker moves under the step
#   7. Tidy added goes before Translate (stage order); moved after it, it is
#      skipped and dimmed, and Move above puts it back
#   8. delete from the row, then Undo: the chain and its use come back
#   9. switched off, the chain stays and a dictation types what was heard
#  10. Diagnostics lists the models with their size; Remove takes the
#      translation away and the chain's step offers Install again
#  11. dictating French: a chain built from the page translates French to
#      English (the fr-en model installed from the language page)
#  12. dictating Chinese: the Chinese script step (built in, no model)
#      types the Simplified the backend heard as Traditional, Taiwan
#  13. dictating Farsi: Translate into Français goes through English (both
#      legs installed in one go), and a language no model reaches from
#      Farsi is not offered
#  14. Settings in French, Simplified Chinese and Farsi (no Farsi catalog
#      yet: English words, right to left): the page, the editor and the
#      language page, as screenshots; the selectors are the translations
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

dict() { on "$LIB; $1"; }
assert_dict() { # description command
    if dict "$2" >/dev/null 2>&1; then echo "PASS: $1"; else echo "FAIL: $1"; FAILED=1; fi
}
evidence() {
    dict "cd myna-shot/out
        cp field.json \"field-chains-$1.json\" 2>/dev/null
        daemon_status > \"status-chains-$1.txt\" 2>&1
        grep -E '^(chains|active-chain)=' $STORE > \"keyfile-chains-$1.txt\" 2>&1
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
now() { on 'date +%s'; }
# slowpoll: poll for up to 5 minutes, for a store download and a restart.
# shellcheck disable=SC2016 # expands in the VM
slowpoll() { printf 'sh:for _ in $(seq 1500); do { %s; } >/dev/null 2>&1 && exit 0; sleep 0.2; done; exit 1' "$1"; }
# The daemon offers a model once it has bound its share (InstalledSteps).
OFFERS='gdbus call --session --dest com.canonical.Myna.Dictation --object-path /com/canonical/Myna/Dictation --method org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation InstalledSteps'
step() { # NN name: a step header
    echo "-- $1. $2"
}

step 0 "the rig: the fake speech backend, a virtual microphone, the field app"
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
assert_dict "the dictation language is English" 'snap run myna.config set language en'
assert_dict "no text snap is installed" '! snap list myna-translation && ! snap list myna-text'

step 1 "with no chain the page says so"
assert_on "the chains are emptied" "snap run myna.config set chains '[]' && snap run myna.config set active-chain ''"
shot waitfor:General tab:Processing "waitfor:No chains yet" shot:shots/chains/01-empty.png \
    || { echo "empty run failed" >&2; FAILED=1; }

step 2 "New chain… makes a chain and opens its editor, empty"
shot waitfor:General tab:Processing "click:New chain…" "waitfor:Add step…" \
    "$(poll "grep -qF \"'$CHAIN', []\" $STORE")" shot:shots/chains/02-editor-empty.png \
    || { echo "new chain run failed" >&2; FAILED=1; }
assert_on "the new chain is stored, empty and not in use" \
    "grep -qF \"'$CHAIN', []\" $STORE && ! grep -q \"active-chain='chain\" $STORE"

step 3 "Add step… lists jobs, not models"
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "waitfor:Tidy up speech" "waitfor:Translate" shot:shots/chains/03-add-step-jobs.png \
    || { echo "add step run failed" >&2; FAILED=1; }

step 4 "Translate opens a searchable language page; Français is added and installed"
T0=$(now)
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" shot:shots/chains/04-language-search.png "click:Français" \
    "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE")" \
    shot:shots/chains/04-installing.png "$(slowpoll "snap list myna-translation")" \
    || { echo "language run failed" >&2; FAILED=1; }
assert_dict "snapd installs the English to French model" \
    'wait_until 300 component_installed myna-translation model-translate-en-fr'
# shellcheck disable=SC2016 # expands in the VM
assert_dict "the install asked for one authorization" "[ \"\$(sudo journalctl -t myna-e2e-polkit --no-pager --since @$T0 -o cat | wc -l)\" = 1 ]"
assert_dict "myna-translation is connected to Myna" \
    'wait_until 60 sh -c "snap connections myna | grep -q \"myna:backend.*myna-translation:provider\""'
evidence 4

step 5 "the daemon restarts itself once, idle, to see the new share"
assert_dict "the daemon left while idle, saying why" \
    "wait_until 60 sh -c \"journalctl --user -u snap.myna.myna.service --no-pager | grep -q 'idle: leaving as asked'\""
assert_dict "systemd started it again, and it is idle" 'wait_until 30 dictation_state_is idle'
evidence 5

step 6 "the chain switched on from its row; a dictation types French"
shot waitfor:General tab:Processing "waitfor:Use $CHAIN" "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain='\" $STORE && ! grep -q \"active-chain=''\" $STORE")" \
    shot:shots/chains/06-in-use.png \
    || { echo "switch on run failed" >&2; FAILED=1; }
# shellcheck disable=SC2016 # expands in the VM
assert_dict "the daemon's chain status is ready" \
    'for _ in $(seq 30); do [ "$(dictation_prop ChainStatus)" = ready ] && exit 0; sleep 1; done; exit 1'
dictate
assert_dict "French is in the field, not the English" \
    "field_get plain | grep -qi '$FRENCH_WORDS' && ! field_is plain '$FULL'"
assert_dict "the daemon bound the translation model" \
    'status_has "model .*myna-translation/.*text\.translate"'
evidence 6
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Stop using" \
    shot:shots/chains/06-editor-in-use.png \
    "sh:xdotool search --onlyvisible --name 'Myna Settings' windowsize 360 760" \
    "waitfor:Translate to" shot:shots/chains/06-editor-360px.png \
    || { echo "narrow editor run failed" >&2; FAILED=1; }

step 7 "Tidy goes before Translate; moved after it, it is skipped, and Move above puts it back"
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "click:Tidy up speech" "click:Install Tidy up speech@$BTN" \
    "$(slowpoll "snap components myna-text 2>/dev/null | grep -q 'model-disfluency-en.*installed'")" \
    "$(slowpoll "$OFFERS | grep -q disfluency")" \
    "waitfor:Add Tidy up speech@$BTN" "click:Add Tidy up speech@$BTN" "waitfor:More actions for tidy" \
    "$(poll "grep -qF \"'$CHAIN', [('tidy', true, {}), ('translate', true\" $STORE")" \
    "click:More actions for tidy" "activate:Move to bottom" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'}), ('tidy', true\" $STORE")" \
    "waitfor:Skipped here" shot:shots/chains/07-skipped.png \
    "click:Move above translate@$BTN" \
    "$(poll "grep -qF \"'$CHAIN', [('tidy', true, {}), ('translate', true\" $STORE")" \
    || { echo "stage order run failed" >&2; FAILED=1; }
assert_on "Tidy runs before Translate again" \
    "grep -qF \"'$CHAIN', [('tidy', true, {}), ('translate', true\" $STORE"

step 8 "delete from the row, then Undo: the chain and its use come back"
shot waitfor:General tab:Processing "click:Delete $CHAIN@$BTN" \
    "$(poll "! grep -qF \"'$CHAIN'\" $STORE")" shot:shots/chains/08-deleted.png \
    "click:Undo@$BTN" "$(poll "grep -qF \"'$CHAIN'\" $STORE")" \
    || { echo "delete run failed" >&2; FAILED=1; }
assert_on "the chain is back and in use" \
    "grep -qF \"'$CHAIN'\" $STORE && ! grep -q \"active-chain=''\" $STORE"

step 9 "switched off, the chain stays and a dictation types what was heard"
shot waitfor:General tab:Processing "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain=''\" $STORE")" shot:shots/chains/09-off.png \
    || { echo "switch off run failed" >&2; FAILED=1; }
assert_on "off keeps the chain" "grep -qF \"'$CHAIN'\" $STORE"
dictate
assert_dict "the dictation lands as heard" "wait_until 10 field_is plain '$FULL'"
evidence 9

step 10 "Diagnostics lists the models with their size; Remove takes the translation away"
shot --polkit allow waitfor:General tab:Diagnostics "waitfor:Speech models" \
    "waitfor:English to Français" "waitfor:In use" shot:shots/chains/10-models.png \
    "click:Remove English to Français@$BTN" \
    "$(poll "! snap components myna-translation | grep -q 'model-translate-en-fr.*installed'")" \
    "gone:English to Français" \
    || { echo "models run failed" >&2; FAILED=1; }
assert_dict "snapd removed the model, not the snap" \
    '! component_installed myna-translation model-translate-en-fr && snap list myna-translation'
assert_on "the chain keeps its step" "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Install" \
    shot:shots/chains/10-step-needs.png \
    || { echo "step needs run failed" >&2; FAILED=1; }
evidence 10

# fresh_chain LANGUAGE TRANSCRIPT: no chains, the dictation language and
# what the fake backend hears; the page's New chain… then makes "New chain".
fresh_chain() {
    assert_on "the chains are emptied" "snap run myna.config set chains '[]' && snap run myna.config set active-chain ''"
    assert_on "the dictation language is $1" "snap run myna.config set language $1"
    assert_on "the fake backend hears the $1 sentence" \
        "sudo snap set myna-fake-backend transcript='$2' && for _ in \$(seq 60); do test -S /var/snap/myna-fake-backend/common/share/provider/myna.sock && exit 0; sleep 0.5; done; exit 1"
}
# translate_to SEARCH NAME COMPONENT...: in the chain's editor, Translate
# into the language the search finds; Settings stays open until snapd has
# each component the choice installs (closing it would drop the request).
translate_to() {
    local search=$1 name=$2 waits=()
    shift 2
    for c in "$@"; do
        waits+=("$(slowpoll "snap components myna-translation | grep -q 'myna-translation+$c *installed'")")
    done
    shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
        "click:Translate" "waitfor:Search languages" "click:Search languages" "type:$search" \
        "gone:Español" "click:$name" "waitfor:Add step…" \
        "$(poll "grep -qF \"('translate', true\" $STORE")" "${waits[@]}" \
        || { echo "translate to $name failed" >&2; FAILED=1; }
}
switch_on() {
    shot waitfor:General tab:Processing "click:Use $CHAIN" \
        "$(poll "! grep -q \"active-chain=''\" $STORE")" \
        || { echo "switch on failed" >&2; FAILED=1; }
}
# shellcheck disable=SC2016 # expands in the VM
READY='for _ in $(seq 120); do [ "$(dictation_prop ChainStatus)" = ready ] && exit 0; sleep 1; done; exit 1'

step 11 "dictating French: a chain translates it to English"
FR_SAID="Je pense que nous devrions partir maintenant."
fresh_chain fr "$FR_SAID"
shot waitfor:General tab:Processing "click:New chain…" "waitfor:Add step…" \
    || { echo "new chain failed" >&2; FAILED=1; }
translate_to engl "English" model-translate-fr-en
assert_dict "snapd installed the French to English model" \
    'component_installed myna-translation model-translate-fr-en'
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:FR →" shot:shots/chains/11-french.png \
    || { echo "french editor failed" >&2; FAILED=1; }
dictate
assert_dict "English is in the field, not the French" \
    "field_get plain | grep -qi 'think' && ! field_is plain '$FR_SAID'"
evidence 11

step 12 "dictating Chinese: Chinese script types Traditional, Taiwan"
ZH_SAID="我认为我们现在应该走了。"
fresh_chain zh "$ZH_SAID"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "click:Chinese script" "click:Add Chinese script@$BTN" "waitfor:Add step…" \
    "$(poll "grep -qF \"('script', true\" $STORE")" \
    "click:Write Chinese as" "waitfor:Traditional, Taiwan (繁體)" "key:Down" "key:Down" "key:Return" \
    "$(poll "grep -qF \"('script', true, {'mode': 'traditional-taiwan'})\" $STORE")" \
    || { echo "script step failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" shot:shots/chains/12-chinese.png \
    || { echo "chinese editor failed" >&2; FAILED=1; }
dictate
assert_dict "Traditional characters are in the field, not the Simplified" \
    "field_get plain | grep -q '認為' && ! field_is plain '$ZH_SAID'"
evidence 12

step 13 "dictating Farsi: Translate into Français through English; Afrikaans is not offered"
FA_SAID="فکر می‌کنم الان باید برویم."
fresh_chain fa "$FA_SAID"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:Afrikaans" \
    "waitfor:No language matches" shot:shots/chains/13-refused.png \
    || { echo "refused language failed" >&2; FAILED=1; }
translate_to fran "Français" model-translate-fa-en model-translate-en-fr
assert_dict "snapd installed both legs: Farsi to English and English to French" \
    'component_installed myna-translation model-translate-fa-en && component_installed myna-translation model-translate-en-fr'
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:FA →" shot:shots/chains/13-farsi-pivot.png \
    || { echo "farsi editor failed" >&2; FAILED=1; }
dictate
assert_dict "French is in the field, not the Farsi" \
    "field_get plain | grep -q '[a-zA-Z]' && ! field_is plain '$FA_SAID'"
evidence 13

step 14 "Settings in French, Simplified Chinese and Farsi"
# What the catalog of LANG says for MSGID, or MSGID with none.
tr_po() { # LANG MSGID
    python3 - "$REPO/client/myna-config/po/$1.po" "$2" <<'PY'
import ast, os, sys
path, msgid = sys.argv[1:]
entries, key, field = {}, None, None
for line in (open(path, encoding="utf-8") if os.path.exists(path) else []):
    line = line.strip()
    if line.startswith("msgid "):
        key, field = ast.literal_eval(line[6:]), "id"
    elif line.startswith("msgstr "):
        entries[key], field = ast.literal_eval(line[7:]), "str"
    elif line.startswith('"') and field == "id":
        key += ast.literal_eval(line)
    elif line.startswith('"') and field == "str":
        entries[key] += ast.literal_eval(line)
print(entries.get(msgid) or msgid)
PY
}
LOCALES=$(mktemp -d)
for po in fr zh_CN; do
    mkdir -p "$LOCALES/$po/LC_MESSAGES"
    msgfmt -o "$LOCALES/$po/LC_MESSAGES/myna-config.mo" "$REPO/client/myna-config/po/$po.po"
done
on 'mkdir -p myna-shot/locale'
for po in fr zh_CN; do
    lxc file push -p --uid 1000 --gid 1000 "$LOCALES/$po/LC_MESSAGES/myna-config.mo" \
        "$VM/home/ubuntu/myna-shot/locale/$po/LC_MESSAGES/myna-config.mo"
done
rm -rf "$LOCALES"
for spec in fr_FR.UTF-8:fr zh_CN.UTF-8:zh_CN fa_IR.UTF-8:fa; do
    loc=${spec%%:*} po=${spec#*:}
    t() { tr_po "$po" "$1"; }
    if on "MYNA_CONFIG_LOCALEDIR=\$HOME/myna-shot/locale myna-shot/shot.sh --display $SHOT_DISPLAY --lang $loc $(printf '%q ' \
        "waitfor:$(t General)" "tab:$(t Processing)" "waitfor:$CHAIN" shot:shots/chains/14-"$po"-page.png \
        "click:$CHAIN" "waitfor:$(t 'Add step…')" shot:shots/chains/14-"$po"-editor.png \
        "click:$(t 'Add step…')" "click:$(t Translate)" "waitfor:$(t 'Search languages')" \
        shot:shots/chains/14-"$po"-languages.png)"; then
        echo "PASS: Settings in $loc shows the page, the editor and the language page"
    else
        echo "FAIL: Settings in $loc"; FAILED=1
    fi
done

dict 'mic_stop' >/dev/null 2>&1
suite_status
