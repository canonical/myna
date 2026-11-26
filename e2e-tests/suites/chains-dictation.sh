#!/usr/bin/env bash
# snapshot: seeded
# tags: processing, chains, dictation
# time: 240s
# A chain at work: the chain switched on from its row types what it says it
# does, in three languages, and switched off the dictation lands as heard.
# The models are already installed (`seeded`), so nothing here waits on the
# store; this is the suite that proves the chain reaches the text field.
#
#   1. a chain switched on from its row types French
#   2. switched off, the chain stays and a dictation types what was heard
#   3. dictating French: a chain translates it to English
#   4. dictating Chinese: Chinese script types Traditional, Taiwan
#   5. dictating Farsi: Translate into Français through English
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig seeded

step 1 "a chain switched on from its row types French"
assert_on "the dictation language is English" "snap run myna.config set language en"
shot --polkit allow waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" "click:Français" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE")" \
    || { echo "translate step failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Stop using" \
    shot:shots/chains-dictation/01-in-use.png \
    "sh:xdotool search --onlyvisible --name 'Myna Settings' windowsize 360 760" \
    "waitfor:Translate to" shot:shots/chains-dictation/01-360px.png \
    || { echo "editor failed" >&2; FAILED=1; }
dictate
assert_dict "French is in the field, not the English" \
    "field_get plain | grep -qi '$FRENCH_WORDS' && ! field_is plain '$FULL'"
assert_dict "the daemon bound the translation model" \
    'status_has "model .*myna-translation/.*text\.translate"'
evidence in-use

step 2 "switched off, the chain stays and a dictation types what was heard"
shot waitfor:General tab:Processing "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain=''\" $STORE")" shot:shots/chains-dictation/02-off.png \
    || { echo "switch off failed" >&2; FAILED=1; }
assert_on "off keeps the chain" "grep -qF \"'$CHAIN'\" $STORE"
dictate
assert_dict "the dictation lands as heard" "wait_until 10 field_is plain '$FULL'"
evidence off

step 3 "dictating French: a chain translates it to English"
FR_SAID="Je pense que nous devrions partir maintenant."
fresh_chain fr "$FR_SAID"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "click:Search languages" "type:engl" \
    "gone:Español" "click:English" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true\" $STORE")" \
    || { echo "translate to English failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:FR → EN" \
    shot:shots/chains-dictation/03-french.png \
    || { echo "french editor failed" >&2; FAILED=1; }
dictate
assert_dict "English is in the field, not the French" \
    "field_get plain | grep -qi 'think' && ! field_is plain '$FR_SAID'"
evidence french

step 4 "dictating Chinese: Chinese script types Traditional, Taiwan"
ZH_SAID="我认为我们现在应该走了。"
fresh_chain zh "$ZH_SAID"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "click:Chinese script" "click:Add Chinese script@$BTN" "waitfor:Add step…" \
    "$(poll "grep -qF \"('script', true\" $STORE")" \
    "click:Write Chinese as@combo box" "waitfor:Traditional, Taiwan (繁體)" \
    "key:Down" "key:Down" "key:Return" \
    "$(poll "grep -qF \"('script', true, {'mode': 'traditional-taiwan'})\" $STORE")" \
    || { echo "script step failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" shot:shots/chains-dictation/04-chinese.png \
    || { echo "chinese editor failed" >&2; FAILED=1; }
dictate
assert_dict "Traditional characters are in the field, not the Simplified" \
    "field_get plain | grep -q '認為' && ! field_is plain '$ZH_SAID'"
evidence chinese

step 5 "dictating Farsi: Translate into Français through English"
FA_SAID="فکر می‌کنم الان باید برویم."
fresh_chain fa "$FA_SAID"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:Afrikaans" \
    "waitfor:No language matches" shot:shots/chains-dictation/05-refused.png \
    || { echo "refused language failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" "click:Français" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true\" $STORE")" \
    || { echo "translate to Français failed" >&2; FAILED=1; }
assert_dict "both legs are installed" \
    'wait_until 60 component_installed myna-translation model-translate-fa-en && component_installed myna-translation model-translate-en-fr'
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:FA → FR" \
    shot:shots/chains-dictation/05-farsi-pivot.png \
    || { echo "farsi editor failed" >&2; FAILED=1; }
dictate
assert_dict "French is in the field, not the Farsi" \
    "field_get plain | grep -q '[a-zA-Z]' && ! field_is plain '$FA_SAID'"
evidence farsi

suite_status
