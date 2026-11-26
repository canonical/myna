#!/usr/bin/env bash
# snapshot: seeded
# tags: processing, chains, locales
# time: 60s
# Settings on the Processing tab in French, Simplified Chinese and Farsi, the
# last with no catalog yet so it runs English words right to left. Each locale
# opens the page, a chain's editor and the language page, taking a checkpoint
# per state for the contact sheet, so a broken translation, a clipped string or
# a mirrored RTL layout is visible there and not only asserted here.
#
#   1. the page, an editor and the language page in each locale
#   2. each locale's own words on the tabs, and its script in the language
#      search: accents folded in French, CJK labels with no spaces in Chinese,
#      Persian letters in Farsi
set -uo pipefail
# shellcheck source=e2e-tests/suites/lib.sh
source "$(dirname "$0")/lib.sh"
# shellcheck source=e2e-tests/suites/chains-lib.sh
source "$(dirname "$0")/chains-lib.sh"

rig seeded

# The catalog's text for a message id, or the English one when it has none.
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

step 1 "the page, an editor and the language page in each locale"
LOCALES=$(mktemp -d)
trap 'rm -rf "$LOCALES"' EXIT
for po in fr zh_CN; do
    mkdir -p "$LOCALES/$po/LC_MESSAGES"
    msgfmt -o "$LOCALES/$po/LC_MESSAGES/myna-config.mo" "$REPO/client/myna-config/po/$po.po"
done
on 'mkdir -p myna-shot/locale'
for po in fr zh_CN; do
    lxc file push -p --uid 1000 --gid 1000 "$LOCALES/$po/LC_MESSAGES/myna-config.mo" \
        "$VM/home/ubuntu/myna-shot/locale/$po/LC_MESSAGES/myna-config.mo"
done
for spec in fr_FR.UTF-8:fr zh_CN.UTF-8:zh_CN fa_IR.UTF-8:fa; do
    loc=${spec%%:*} po=${spec#*:}
    t() { tr_po "$po" "$1"; }
    # One chain to open, made before the app starts so the page lists it. Its
    # Translate step puts the editor's "EN →" badge - a left-to-right run -
    # on Farsi's right-to-left page.
    # shellcheck disable=SC2016 # expands in the VM
    assert_on "a chain to open" \
        "snap run myna.config set chains \"[('chain-1', '$CHAIN', [('translate', true, {'to': 'fr'})])]\""
    if on "MYNA_CONFIG_LOCALEDIR=\$HOME/myna-shot/locale myna-shot/shot.sh --display $SHOT_DISPLAY --lang $loc $(printf '%q ' \
        "waitfor:$(t General)" "tab:$(t Processing)" "waitfor:$CHAIN" \
        shot:"shots/chains-locales/01-$po-page.png" \
        "click:$CHAIN" "waitfor:$(t 'Add step…')" "waitfor:EN →" \
        shot:"shots/chains-locales/01-$po-editor.png" \
        "click:$(t 'Add step…')" "click:$(t Translate)" "waitfor:Français" \
        shot:"shots/chains-locales/01-$po-languages.png")"; then
        echo "PASS: Settings in $loc shows the page, the editor and the language page"
    else
        echo "FAIL: Settings in $loc"; FAILED=1
    fi
done

step 2 "each locale's own words, and its script in the language search"
# A `tr_po` selector falls back to the msgid, so a catalog that never loaded
# would pass it. The tabs are in the catalog: French and Chinese translate
# them, and Farsi, which has none, must read English.
shot_locale() { # LOCALE STEP...: the app in LOCALE, driven through STEPs
    local locale=$1
    shift
    on "MYNA_CONFIG_LOCALEDIR=\$HOME/myna-shot/locale myna-shot/shot.sh --display $SHOT_DISPLAY --lang $locale $(printf '%q ' "$@")"
}
# The language page from a new chain of its own: the chain step 1 opens
# translates into French, so from there the page offers what French reaches,
# not what English does.
WIRE=("tab:Processing" "click:New chain…" "waitfor:Add step…"
    "click:Add step…" "waitfor:Translate" "click:Translate"
    "waitfor:Search languages" "click:Search languages")
if shot_locale fr_FR.UTF-8 "waitfor:Général" "gone:General" "${WIRE[@]}" \
    "type:francais" "waitfor:Français" "gone:Deutsch" \
    shot:shots/chains-locales/02-fr-search.png; then
    echo "PASS: French tabs, and an accent-folded search finds Français"
else
    echo "FAIL: French tabs or the accent-folded search"; FAILED=1
fi
# The Processing page's own strings are not in the catalog yet (the .po files
# are behind the template), so the tabs are the CJK it has: two characters,
# no spaces. The search is by code, which is what narrows the list there - and
# it is Japanese that narrows it, not Chinese: the language page offers no
# Chinese target at all (see this suite's note at the end of step 2).
if shot_locale zh_CN.UTF-8 "waitfor:常规" "waitfor:诊断" "gone:General" "${WIRE[@]}" \
    "type:ja" "waitfor:日本語" "gone:Deutsch" \
    shot:shots/chains-locales/02-zh-search.png; then
    echo "PASS: the Chinese tabs are CJK with no spaces, and a code search narrows"
else
    echo "FAIL: Chinese tabs or the search"; FAILED=1
fi
# Farsi has no catalog, so its words are English on a right-to-left page; the
# search takes Persian letters, which is the script half of that.
if shot_locale fa_IR.UTF-8 "waitfor:General" "${WIRE[@]}" \
    "type:فارسی" "waitfor:فارسی" "gone:Deutsch" \
    shot:shots/chains-locales/02-fa-search.png; then
    echo "PASS: Farsi reads English right to left, and فارسی finds the row"
else
    echo "FAIL: Farsi fallback or search"; FAILED=1
fi
# Not asserted, because it cannot hold today: the language page offers no
# Chinese target from English. `chains_view::stranded` judges the step it would
# insert by `fits`'s deduced output, which reduces zh-Hans to the bare `zh`,
# and no catalog entry covers `zh`, so both Chinese rows are filtered out
# although `step_catalog::languages` lists them and the editor's target picker
# (which passes `strands: &|_| false`) offers them. Whether the page should
# offer them is a product call - a translate step into Chinese has to name the
# script the Chinese script step writes - so it is in the task Handoff rather
# than guessed at here.

suite_status
