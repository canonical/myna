# myna-desktop translations

The desktop client's user-visible strings live in this `po/` directory, in the
gettext domain **`myna-desktop`** (R25).

## What's translatable

- The publisher-owned status strings for the dictation states
  (`src/indicator/mod.rs`) — translated before publication
  in the D-Bus `StatusMessage` property, so every consumer sees the same
  final label.
- The user-facing error templates this crate owns: the backend-socket
  resolution errors (`src/backend.rs`) and the injection errors
  (`src/inject/mod.rs`), translated at the point their `Display` is rendered.

Backend transport errors (e.g. "cannot reach backend") are owned by the
`myna-orchestrator` crate and live in its own `po/` (see
`client/myna-orchestrator/po/README.md`).

## Re-extracting the template

`po/myna-desktop.pot` is generated from the Rust sources (xgettext treats `.rs` as
C-ish, which is sufficient for `gettext()` calls). Regenerate with:

```sh
xgettext --from-code=UTF-8 --keyword=gettext \
  --add-comments=TRANSLATORS \
  --output=po/myna-desktop.pot \
  --files-from=po/POTFILES.in
```

`POTFILES.in` lists every source file with translatable strings; keep it in
sync when files are added or removed.

## Adding a language

1. Create `<lang>.po` from the template: `msginit --input=po/myna-desktop.pot
   --locale=<lang> --output-file=po/<lang>.po`
2. Add `<lang>` to `po/LINGUAS` (one code per line).
3. Compile with `msgfmt --check --check-format po/<lang>.po -o
   /usr/share/locale/<lang>/LC_MESSAGES/myna-desktop.mo` (the snap stages it under
   `$SNAP/share/locale`; a build-tree run can point `MYNA_DESKTOP_LOCALEDIR` at
   its own directory).

The domain is bound by `myna-desktop` before it publishes D-Bus status labels.
With no .mo installed, gettext is the identity function — localization failure
is never a crash.
