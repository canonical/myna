#!/usr/bin/env bash
# snapshot: ready
# tags: processing, chains
# time: 150s
# The page Translate opens: a search over the languages the text can be
# translated into, what each costs, what is reachable, and the installed
# translation models below it. `ready` has no components, so sizes and the
# install are real.
#
#   1. Translate opens the language page, named after the language the text is
#      in where the step would go
#   2. the search matches an endonym, the interface language's name and the code
#      (German finds Deutsch; Francais finds Français with its cedilla)
#   3. a language no model reaches from here is not listed
#   4. choosing a language installs it and adds the step at its stage
#   5. installed translation models offers Remove
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig ready

step 1 "Translate opens the language page"
shot waitfor:General tab:Processing "click:New chain…" "click:Add step…" \
    "waitfor:Translate" "click:Translate" "waitfor:Search languages" \
    shot:shots/processing-language/01-page.png \
    || { echo "language page failed" >&2; FAILED=1; }

step 2 "the search matches an endonym, the interface language's name and the code"
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" shot:shots/processing-language/02-endonym.png \
    || { echo "endonym search failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:French" \
    "waitfor:Français" "gone:Español" \
    || { echo "exonym search failed" >&2; FAILED=1; }
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:fa" \
    "waitfor:Français" shot:shots/processing-language/02-code.png \
    || { echo "code search failed" >&2; FAILED=1; }
# The interface's own name for a language finds the row titled in that
# language, which is the whole point of the exonym table behind it.
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:German" \
    "waitfor:Deutsch" "gone:Español" shot:shots/processing-language/02-exonym.png \
    || { echo "exonym search failed" >&2; FAILED=1; }

step 3 "a language no model reaches from here is not listed"
shot waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" "click:Translate" \
    "waitfor:Search languages" "click:Search languages" "type:Afrikaans" \
    "waitfor:No language matches" shot:shots/processing-language/03-refused.png \
    || { echo "refused language failed" >&2; FAILED=1; }

step 4 "choosing a language installs it and adds the step at its stage"
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "click:Search languages" "type:fran" \
    "gone:Español" shot:shots/processing-language/04-search.png "click:Français" \
    "waitfor:Add step…" \
    "$(poll "grep -qF \"('translate', true, {'to': 'fr'})\" $STORE")" \
    shot:shots/processing-language/04-installing.png \
    "$(slowpoll "snap list myna-translation")" \
    || { echo "language install failed" >&2; FAILED=1; }
assert_dict "snapd installed the English to French model" \
    'wait_until 300 component_installed myna-translation model-translate-en-fr'

step 5 "installed translation models offers Remove"
# The group lists what the daemon offers, so wait for its own answer first.
if offered text; then echo "PASS: the daemon offers the translation model"; else
    echo "FAIL: the daemon does not offer the translation model"; FAILED=1
fi
shot --polkit allow waitfor:General tab:Processing "click:$CHAIN" "click:Add step…" \
    "click:Translate" "waitfor:Search languages" "waitfor:English to Français" \
    shot:shots/processing-language/05-installed.png \
    || { echo "installed models failed" >&2; FAILED=1; }

evidence language
suite_status
