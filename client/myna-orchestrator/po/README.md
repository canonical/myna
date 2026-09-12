# myna-orchestrator translations

User-visible strings owned by this crate live in this `po/` directory, in the
gettext domain **`myna-orchestrator`**. Today that is the `BackendError`
templates rendered by `Display` (`src/backend/mod.rs`).

The desktop package (and any embedding application) initializes this domain at
startup via `myna_orchestrator::i18n::bind`, binding it to the same locale
tree it resolves for its own domain. A crate consumer that never binds the
domain gets gettext's identity function — always safe.

## Re-extracting the template

`po/myna-orchestrator.pot` is generated from the Rust sources (xgettext treats
`.rs` as C-ish, which is sufficient for `gettext()`/`tr()` calls). The crate's
translation helper is named `tr`, so extraction must list it as a keyword.
Regenerate with:

```sh
xgettext --from-code=UTF-8 --keyword=gettext --keyword=tr \
  --add-comments=TRANSLATORS \
  --output=po/myna-orchestrator.pot \
  --files-from=po/POTFILES.in
```

`POTFILES.in` lists every source file with translatable strings; keep it in
sync when files are added or removed.

## Adding a language

1. Create `<lang>.po` from the template: `msginit --input=po/myna-orchestrator.pot
   --locale=<lang> --output-file=po/<lang>.po`
2. Add `<lang>` to `po/LINGUAS` (one code per line).
3. Compile with `msgfmt --check --check-format po/<lang>.po -o
   <locale-root>/<lang>/LC_MESSAGES/myna-orchestrator.mo`, installing the
   compiled file under the same locale root the desktop resolves (e.g.
   `$SNAP/usr/share/locale` in the snap).
