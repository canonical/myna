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
    # One chain to open, made before the app starts so the page lists it.
    assert_on "a chain to open" "snap run myna.config set chains \"[('chain-1', '$CHAIN', [('tidy', true, {})])]\""
    if on "MYNA_CONFIG_LOCALEDIR=\$HOME/myna-shot/locale myna-shot/shot.sh --display $SHOT_DISPLAY --lang $loc $(printf '%q ' \
        "waitfor:$(t General)" "tab:$(t Processing)" "waitfor:$CHAIN" \
        shot:"shots/chains-locales/01-$po-page.png" \
        "click:$CHAIN" "waitfor:$(t 'Add step…')" \
        shot:"shots/chains-locales/01-$po-editor.png" \
        "click:$(t 'Add step…')" "click:$(t Translate)" "waitfor:Français" \
        shot:"shots/chains-locales/01-$po-languages.png")"; then
        echo "PASS: Settings in $loc shows the page, the editor and the language page"
    else
        echo "FAIL: Settings in $loc"; FAILED=1
    fi
done

suite_status
