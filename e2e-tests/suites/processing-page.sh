#!/usr/bin/env bash
# snapshot: seeded
# tags: processing, chains
# time: 90s
# The Processing page itself: the chain list, which chain is in use, making a
# chain, and deleting one with the Undo toast. No dictation and no store: the
# state is the keyfile, and every case reads it back.
#
#   1. Get more steps with no chain makes one, fills it and uses it
#   2. New chain… makes an empty chain and opens it, not in use
#   3. a chain is switched on from its row and off again, one at a time
#   4. delete from the row, then Undo: the chain and its use come back
#   5. a chain that was off comes back off
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig seeded

step 1 "Get more steps with no chain makes one, fills it and uses it"
shot waitfor:General tab:Processing "click:Get more steps" "waitfor:Tidy up speech" \
    "click:Add Tidy up speech@$BTN" \
    "$(poll "grep -qF \"('tidy', true, {})\" $STORE")" \
    shot:shots/processing-page/01-add-no-chain.png \
    || { echo "add with no chain failed" >&2; FAILED=1; }
assert_on "a chain was made, holds the step and is in use" \
    "grep -qF \"('tidy', true, {})\" $STORE && ! grep -q \"active-chain=''\" $STORE"

step 2 "New chain… makes an empty chain and opens it, not in use"
assert_on "the chains are emptied" \
    "snap run myna.config set chains '[]' && snap run myna.config set active-chain ''"
shot waitfor:General tab:Processing "click:New chain…" "waitfor:Add step…" \
    "$(poll "grep -qF \"'$CHAIN', []\" $STORE")" \
    shot:shots/processing-page/02-editor-empty.png \
    || { echo "new chain run failed" >&2; FAILED=1; }
assert_on "the new chain is stored, empty and not in use" \
    "grep -qF \"'$CHAIN', []\" $STORE && ! grep -q \"active-chain='chain\" $STORE"

step 3 "a chain is switched on from its row and off again, one at a time"
shot waitfor:General tab:Processing "click:New chain…" "waitfor:Add step…" \
    "$(poll "grep -qF \"'New chain 2', []\" $STORE")" \
    || { echo "second chain failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain='\" $STORE")" "click:Use New chain 2" \
    "$(poll "grep -q \"active-chain='\" $STORE")" \
    shot:shots/processing-page/03-in-use.png \
    || { echo "switch run failed" >&2; FAILED=1; }
assert_on "exactly one chain is in use" \
    "[ \$(grep -c \"active-chain='chain\" $STORE) = 1 ]"
shot waitfor:General tab:Processing "click:Use New chain 2" \
    "$(poll "grep -q \"active-chain=''\" $STORE")" \
    shot:shots/processing-page/03-off.png \
    || { echo "switch off run failed" >&2; FAILED=1; }
assert_on "off keeps the chain" "grep -qF \"'New chain 2'\" $STORE"

step 4 "delete from the row, then Undo: the chain and its use come back"
shot waitfor:General tab:Processing "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain='\" $STORE")" "checked:Use $CHAIN" \
    || { echo "switch on failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:Delete $CHAIN@$BTN" \
    "$(poll "! grep -qF \"'$CHAIN'\" $STORE")" shot:shots/processing-page/04-deleted.png \
    "waitfor:Undo" "wait:1" "click:Undo@$BTN" \
    || { echo "delete run failed" >&2; FAILED=1; }
# The toast's Undo is a real button, but it carries no AT-SPI action, so
# `activate:` clicks nothing and the chain stays deleted. Assert the page and
# the store: the row is back, its switch on, and the app has written it.
shot waitfor:General tab:Processing "waitfor:Use $CHAIN" "checked:Use $CHAIN" \
    shot:shots/processing-page/04-undone.png \
    || { echo "undo did not restore the chain" >&2; FAILED=1; }
assert_soon "the chain is back and in use" \
    "grep -qF \"'$CHAIN'\" $STORE && ! grep -q \"active-chain=''\" $STORE"

step 5 "a chain that was off comes back off"
shot waitfor:General tab:Processing "checked:Use $CHAIN" "click:Use $CHAIN" \
    "$(poll "grep -q \"active-chain=''\" $STORE")" "unchecked:Use $CHAIN" \
    "click:Delete $CHAIN@$BTN" "$(poll "! grep -qF \"'$CHAIN'\" $STORE")" \
    "waitfor:Undo" "wait:1" "click:Undo@$BTN" "waitfor:Use $CHAIN" "unchecked:Use $CHAIN" \
    || { echo "undo of an off chain failed" >&2; FAILED=1; }
# Back, and off: the row is there with its switch off, and the store holds the
# chain with nothing in use.
shot waitfor:General tab:Processing "waitfor:Use $CHAIN" "unchecked:Use $CHAIN" \
    shot:shots/processing-page/05-undone-off.png \
    || { echo "undo did not restore the off chain" >&2; FAILED=1; }
assert_soon "the chain is back and nothing is in use" \
    "grep -qF \"'$CHAIN'\" $STORE && grep -q \"active-chain=''\" $STORE"

evidence page
suite_status
