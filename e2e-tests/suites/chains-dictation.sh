#!/usr/bin/env bash
# snapshot: seeded
# tags: processing, chains, dictation
# time: 320s
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
#   6. dictating English: Translate into 简体中文 types Simplified Chinese
#   7. dictating Chinese: Translate into English converts the script first
#      and types English
#   8. a chain holds the sentence being said and the one before it: the
#      first reaches the field while the dictation goes on, the rest at the
#      stop
#   9. focus lost while text is held: the held sentences are not typed
#      anywhere, and Past dictations records the loss
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

# The models name the target zh-Hans while the language after the step is the
# bare zh: the page used to drop the row, and the daemon asked for en-zh.
step 6 "dictating English: Translate into 简体中文 types Simplified Chinese"
fresh_chain en "$FULL"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:zh" \
    "gone:Español" "click:简体中文" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'zh-Hans'})\" $STORE")" \
    || { echo "translate to 简体中文 failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:EN → ZH" \
    shot:shots/chains-dictation/06-simplified.png \
    || { echo "simplified editor failed" >&2; FAILED=1; }
dictate
# 狐狸 is fox; the field holds no Latin letter.
assert_dict "Simplified Chinese is in the field, not the English" \
    "field_get plain | grep -q '狐狸' && ! field_get plain | grep -q '[a-zA-Z]'"
evidence simplified

# The only model reads zh-Hant and the dictation is mixed-script zh, so the
# solver inserts the conversion; the editor says so under the step.
step 7 "dictating Chinese: Translate into English converts the script first"
fresh_chain zh "$ZH_SAID"
assert_dict "the Chinese to English model is installed" \
    'wait_until 60 component_installed myna-translation model-translate-zh-hant-en'
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:engl" \
    "gone:Español" "click:English" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'en'})\" $STORE")" \
    || { echo "translate to English failed" >&2; FAILED=1; }
switch_on
assert_dict "the daemon's chain status is ready" "$READY"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:ZH → EN" \
    "waitfor:Writes Traditional first" shot:shots/chains-dictation/07-chinese-english.png \
    || { echo "chinese to english editor failed" >&2; FAILED=1; }
dictate
assert_dict "English is in the field, no Chinese" \
    "field_get plain | grep -q '[a-zA-Z]' && ! field_get plain | LC_ALL=C.UTF-8 grep -qP '\\p{Han}'"
evidence chinese-english

# The fake hears three sentences a second apart. A chain holds the sentence
# being said and the last finished one (either may still be revised), so the
# first is typed once the third arrives and the other two at the stop.
HELD="One fine morning we left.|Two hours later it rained.|Three of us got wet."
use_tidy() {
    assert_on "a Tidy chain is in use" \
        "snap run myna.config set chains \"[('chain-1', '$CHAIN', [('tidy', true, {})])]\" && snap run myna.config set active-chain chain-1"
    assert_dict "the daemon's chain status is ready" "$READY"
}
start_held() {
    assert_dict "the field is cleared" 'field_clear'
    assert_dict "the plain field is focused afresh" 'field_focus other && field_focus plain'
    dict dictation_toggle
    assert_dict "recording" 'wait_until 10 dictation_state_is recording'
    assert_dict "the first sentence is typed while recording" \
        'wait_until 15 field_has plain morning && dictation_state_is recording'
    assert_dict "the second and third are held" '! field_has plain rained && ! field_has plain wet'
}

step 8 "a chain holds the last two sentences, and types them at the stop"
fresh_chain en "$HELD"
use_tidy
start_held
dict dictation_toggle
assert_dict "idle after the stop" 'wait_until 20 dictation_state_is idle'
assert_dict "the held sentences are typed at the stop" \
    'wait_until 10 field_has plain wet && field_has plain rained'
evidence held

step 9 "focus lost while text is held: nothing more is typed, and it is recorded"
assert_on "Past dictations keeps a record" "snap run myna.config set processing-history true"
use_tidy
start_held
assert_dict "focus moves to the other field" 'field_focus other'
assert_dict "the dictation ends on the focus loss" 'wait_until 10 dictation_state_is idle'
assert_dict "the held sentences went nowhere" \
    "sleep 2; ! field_has plain rained && ! field_has plain wet && field_is other ''"
assert_dict "Past dictations records the focus loss" \
    "tail -n 1 \$HOME/snap/myna/common/processing-history.jsonl | grep -q '\"outcome\":\"focus-lost\"'"
evidence held-focus-lost

suite_status
