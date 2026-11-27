# myna-config translations

User-visible strings use the `myna-config` gettext domain: `gettext()` in
Rust, `_()` in Blueprint, the `<summary>` elements of the shared GSettings
schema, the AppStream metainfo's name, summary and description, and the
desktop entry. Regenerate the template with `make i18n` from the repository
root (`dev/i18n.sh` lists the crate); `make check` fails while the committed
template is stale.

`LINGUAS` lists the shipped languages. The deb build compiles each catalog and
merges it into the desktop entry and metainfo
(`build/install-translations.sh`).

Set `MYNA_CONFIG_LOCALEDIR` to a gettext localedir (`DIR/<lang>/LC_MESSAGES/myna-config.mo`,
what `msgfmt -o` into such a tree makes) to test a catalog outside the
system locale directories, with `LANG` set to that language. Without an installed catalog, gettext safely returns each source
string unchanged.
