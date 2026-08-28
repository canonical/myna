# myna-hud translations

The renderer's user-visible strings live in this `po/` directory, in the
gettext domain **`myna`** (R25).

## What's translatable

- The status strings for the dictation states (`states.rs`) — the msgids are
  marked with `n_()` (a no-op marker, the port of gettext's `N_`), since
  they're looked up by variable; this is what makes them extractable.
- The development-lab UI strings (`lab.rs`) — wrapped directly in
  `gettext()`.

## Re-extracting the template

`po/myna.pot` is generated from the Rust sources (xgettext treats `.rs` as
C-ish, which is sufficient for `gettext()`/`n_()` calls). Regenerate with:

```sh
xgettext --from-code=UTF-8 --keyword=gettext --keyword=n_ \
  --add-comments=TRANSLATORS \
  --output=po/myna.pot \
  --files-from=po/POTFILES.in
```

`POTFILES.in` lists every source file with translatable strings; keep it in
sync when files are added or removed.

## Adding a language

1. Create `<lang>.po` from the template: `msginit --input=po/myna.pot
   --locale=<lang> --output-file=po/<lang>.po`
2. Add `<lang>` to `po/LINGUAS` (one code per line).
3. Compile with `msgfmt --check --check-format po/<lang>.po -o
   /usr/share/locale/<lang>/LC_MESSAGES/myna.mo` (the snap stages it under
   `$SNAP/share/locale`; a build-tree run can point `MYNA_HUD_LOCALEDIR` at
   its own directory).

The domain is bound in `main.rs` via `gettextrs::TextDomain::new("myna")`.
With no .mo installed, gettext is the identity function — localization
failure is never a crash.