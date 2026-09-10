#!/bin/bash
# i18n.sh - regenerate the translation templates, or check they are fresh.
#
# One .pot per translated crate, extracted from the files each crate's
# po/POTFILES.in lists. Without arguments this rewrites the committed
# templates (`make i18n`). With --check it extracts to a scratch file and diffs
# against the committed one, ignoring the creation-date header xgettext stamps
# on every run, so the static battery (`make check`) fails when a translatable
# string changed and nobody re-extracted. Nothing here runs msgmerge: the .po
# catalogs are updated by translators, not by this script.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# crate directory : extra --keyword flags (gettext is always a keyword)
CRATES=(
    "client/myna-desktop:"
    "client/myna-orchestrator:--keyword=tr"
)

check=0
case "${1:-}" in
    "") ;;
    --check) check=1 ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

# gettext 0.23 has no Rust lexer, so the C one reads the sources: it extracts
# gettext("...") calls correctly (the committed templates are its output), but
# it does not know lifetimes, and every `'static` / `'_` is an "unterminated
# character constant" to it. Name the language so xgettext stops guessing it
# from the extension, and drop that one warning; anything else it says is
# still shown. Switch to --language=Rust once the workshop's gettext has it.
extract() {
    local crate=$1 keywords=$2 out=$3
    # shellcheck disable=SC2086  # keywords is a space-separated flag list
    (cd "$REPO_ROOT/$crate" && xgettext --from-code=UTF-8 --language=C --keyword=gettext $keywords \
        --add-comments=TRANSLATORS --files-from=po/POTFILES.in --output="$out" \
        2> >(grep -v 'unterminated character constant' >&2))
}

# The template minus what changes without any string changing: the creation
# date xgettext stamps on every run, and the `#: file:line` references, which
# move whenever a line is inserted above a call. Both stay in the committed
# file for translators; they just do not count as drift.
stable() {
    grep -v -e '^"POT-Creation-Date:' -e '^#:' "$1"
}

stale=0
for entry in "${CRATES[@]}"; do
    crate=${entry%%:*}
    keywords=${entry#*:}
    pot="$REPO_ROOT/$crate/po/$(basename "$crate").pot"
    if [ "$check" -eq 0 ]; then
        extract "$crate" "$keywords" "$pot"
        echo "wrote $crate/po/$(basename "$pot")"
        continue
    fi
    scratch=$(mktemp)
    extract "$crate" "$keywords" "$scratch"
    if ! diff -u --label "$crate/po/$(basename "$pot") (committed)" --label "(extracted)" \
            <(stable "$pot") <(stable "$scratch"); then
        echo "(line references and the creation date are not compared)" >&2
        stale=1
    fi
    rm -f "$scratch"
done

if [ "$stale" -ne 0 ]; then
    echo "i18n-check: template(s) stale; run \`make i18n\` and commit the result" >&2
    exit 1
fi
[ "$check" -eq 0 ] || echo "i18n-check: templates fresh"
