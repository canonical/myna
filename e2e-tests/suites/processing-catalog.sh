#!/usr/bin/env bash
# snapshot: ready
# tags: processing, chains
# time: 180s
# The catalog page and the install path it owns: install from an entry, add
# the step, remove the model from the entry, and Diagnostics listing what is
# installed. `ready` is the base without text components, so the download here
# is real, and this is the suite that pays for it.
#
#   1. the catalog lists jobs, not models; Install takes the component
#   2. Add puts the step in the chain at its stage
#   3. Diagnostics lists the model with its size
#   4. Remove takes the component away and the step stays, offering Install
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig ready

step 1 "the catalog lists jobs, not models; Install takes the component"
T0=$(now)
shot --polkit allow waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "waitfor:Tidy up speech" "waitfor:Translate" \
    shot:shots/processing-catalog/01-jobs.png \
    "click:Install Tidy up speech@$BTN" \
    "$(slowpoll "snap components myna-text 2>/dev/null | grep -q 'model-disfluency-en *installed'")" \
    "$(slowpoll "$OFFERS | grep -q disfluency")" \
    || { echo "catalog install failed" >&2; FAILED=1; }
assert_dict "snapd installed the disfluency component" \
    'wait_until 60 component_installed myna-text model-disfluency-en'
# shellcheck disable=SC2016 # expands in the VM
assert_dict "the install asked for one authorization" \
    "[ \"\$(sudo journalctl -t myna-e2e-polkit --no-pager --since @$T0 -o cat | wc -l)\" = 1 ]"

step 2 "Add puts the step in the chain at its stage"
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "waitfor:Add Tidy up speech@$BTN" "click:Add Tidy up speech@$BTN" \
    "$(poll "grep -qF \"('tidy', true, {})\" $STORE")" \
    shot:shots/processing-catalog/02-added.png \
    || { echo "add failed" >&2; FAILED=1; }
assert_on "the step is in the chain" "grep -qF \"('tidy', true, {})\" $STORE"

step 3 "Diagnostics lists the model with its size"
shot waitfor:General tab:Diagnostics "waitfor:Speech models" "waitfor:Text models" \
    shot:shots/processing-catalog/03-diagnostics.png \
    || { echo "diagnostics failed" >&2; FAILED=1; }

step 4 "Remove takes the component away and the step stays, offering Install"
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" \
    "waitfor:More actions for tidy" "click:More actions for tidy" "activate:Remove model" \
    "$(slowpoll "! snap components myna-text 2>/dev/null | grep -q 'model-disfluency-en *installed'")" \
    shot:shots/processing-catalog/04-removed.png \
    || { echo "remove failed" >&2; FAILED=1; }
assert_dict "snapd removed the component, not the snap" \
    '! component_installed myna-text model-disfluency-en && snap list myna-text'
assert_on "the chain keeps its step" "grep -qF \"('tidy', true, {})\" $STORE"
shot waitfor:General tab:Processing "click:$CHAIN" "waitfor:Install" \
    shot:shots/processing-catalog/04-step-needs.png \
    || { echo "step needs failed" >&2; FAILED=1; }

evidence catalog
suite_status
