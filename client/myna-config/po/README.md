# myna-config translations

User-visible strings use the `myna-config` gettext domain. Regenerate the
template from the crate root with:

```sh
xgettext --from-code=UTF-8 --keyword=gettext \
  --add-comments=TRANSLATORS \
  --output=po/myna-config.pot \
  --files-from=po/POTFILES.in
```

Set `MYNA_CONFIG_LOCALEDIR` to test a catalog outside the system locale
directories. Without an installed catalog, gettext safely returns each source
string unchanged.
