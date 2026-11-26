#!/usr/bin/env bash
# snapshot: seeded
# tags: processing, chains
# time: 120s
# A chain's editor: adding a step, where it lands, what a step's own controls
# do, and the narrow layout. The text models are already installed, so nothing
# here waits on the store.
#
#   1. Add step… lists jobs, not models
#   2. a step added from the editor goes in at its stage
#   3. moved after Translate it is skipped and dimmed; Move above puts it back
#   4. a step's own switch pauses only that step
#   5. the step's input badge and picker, and the picker under it at 360 px
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig seeded

step 1 "Add step… lists jobs, not models"
shot waitfor:General tab:Processing "click:New chain…" "waitfor:Add step…" "click:Add step…" \
    "waitfor:Tidy up speech" "waitfor:Translate" \
    shot:shots/processing-editor/01-add-step-jobs.png \
    || { echo "add step run failed" >&2; FAILED=1; }

step 2 "a step added from the editor goes in at its stage"
# The language page adds the Translate step at its stage's place; Tidy then
# goes before it, which the keyfile shows.
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Add step…" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" "click:Français" "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE")" \
    || { echo "translate added failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Add step…" "click:Add step…" \
    "waitfor:Tidy up speech" "click:Add Tidy up speech@$BTN" \
    "$(poll "grep -qF \"('tidy', true, {}), ('translate', true\" $STORE")" \
    shot:shots/processing-editor/02-stage-order.png \
    || { echo "stage order failed" >&2; FAILED=1; }
assert_on "Tidy runs before Translate" \
    "grep -qF \"('tidy', true, {}), ('translate', true\" $STORE"

step 3 "moved after Translate it is skipped and dimmed; Move above puts it back"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:More actions for Tidy up speech" \
    "click:More actions for Tidy up speech" "activate:Move to bottom" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'}), ('tidy', true\" $STORE")" \
    "waitfor:Skipped" shot:shots/processing-editor/03-skipped.png \
    "click:Move above Translate@$BTN" \
    "$(poll "grep -qF \"('tidy', true, {}), ('translate', true\" $STORE")" \
    || { echo "skipped and fixed failed" >&2; FAILED=1; }
assert_on "Tidy runs before Translate again" \
    "grep -qF \"('tidy', true, {}), ('translate', true\" $STORE"

step 4 "a step's own switch pauses only that step"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Use Tidy up speech" \
    "click:Use Tidy up speech" "$(poll "grep -qF \"('tidy', false\" $STORE")" \
    shot:shots/processing-editor/04-paused.png \
    || { echo "step switch failed" >&2; FAILED=1; }
assert_on "only that step was paused" \
    "grep -qF \"('tidy', false\" $STORE && grep -qF \"('translate', true\" $STORE"

step 5 "the step's input badge and picker, and the picker under it at 360 px"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:EN → FR" \
    shot:shots/processing-editor/05-wide.png \
    "sh:xdotool search --onlyvisible --name 'Myna Settings' windowsize 360 760" \
    "waitfor:Translate to" shot:shots/processing-editor/05-360px.png \
    || { echo "narrow editor failed" >&2; FAILED=1; }

evidence editor
suite_status
