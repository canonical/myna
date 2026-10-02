#!/bin/bash
# purge.sh - return a machine to "Myna was never installed", for testing a
# fresh install and the onboarding wizard end to end.
#
# Removes, for the user running it:
#   - every installed myna* snap, with --purge (data, snapshots, components)
#   - the `experimental.user-daemons` snapd flag onboarding asks the user to set
#   - the GNOME custom keybinding myna-config installs where there is no
#     GlobalShortcuts portal, and the portal's stored grants under every app
#     id Myna has had (myna_myna, and "." from a portal that lost the id)
#   - Myna Settings' dconf keys and notification entry
#   - hand-installed desktop files, icons and a ~/.local shell extension copy
#   - pre-rename leftovers: /org/myna/ in dconf, ~/.config/myna, and an
#     unpackaged org.myna.dictation schema
# The myna-config deb stays unless --deb: it is what a fresh install starts
# from. ~/.cache/myna (model downloads) always stays.
#
#   dev/purge.sh --dry-run   # print what would go
#   dev/purge.sh             # ask once, then purge
#   dev/purge.sh --yes --deb # no question; also purge the myna-config deb
#
# Log out and back in afterwards: gnome-shell and gsd-media-keys keep the
# shortcut grabs they already hold until then.
set -euo pipefail

DRY=0
YES=0
DEB=0
for arg in "$@"; do
    case $arg in
        -n | --dry-run) DRY=1 ;;
        -y | --yes) YES=1 ;;
        --deb) DEB=1 ;;
        *)
            sed -n '2,/^set /p' "$0" | sed '$d; s/^# \{0,1\}//'
            exit 2
            ;;
    esac
done

KEYBINDING=/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/
GLOBAL_SHORTCUTS=/org/gnome/settings-daemon/global-shortcuts/
PORTAL_APP=myna_myna
NOTIFICATIONS=/org/gnome/desktop/notifications/
NOTIFICATION_APP=com-canonical-myna-config
EXTENSION=myna-shell@canonical.com
SCHEMA_DIR=/usr/share/glib-2.0/schemas
LEGACY_SCHEMA=$SCHEMA_DIR/org.myna.dictation.gschema.xml

run() {
    printf '  %s\n' "$*"
    [ "$DRY" -eq 1 ] || "$@"
}

# Whether a dpkg-owned copy of the extension stays installed: the myna-config
# deb's under /usr/share/gnome, or Ubuntu's. It keeps its enablement; a
# hand-made copy does not. The deb's own copy does not count under --deb.
# /usr/share/ubuntu and /usr/share/gnome are named because this may run
# outside a session's environment.
packaged_extension() {
    local dir path owner
    local IFS=:
    for dir in ${XDG_DATA_DIRS:-/usr/local/share:/usr/share} /usr/share/ubuntu /usr/share/gnome; do
        # dpkg -S matches paths literally: /usr/share/ in XDG_DATA_DIRS
        # would make a // it never finds.
        path=${dir%/}/gnome-shell/extensions/$EXTENSION
        [ -e "$path" ] || continue
        owner=$(dpkg -S "$path" 2>/dev/null | cut -d: -f1) || continue
        [ -n "$owner" ] || continue
        if [ "$DEB" -eq 1 ] && [ "$owner" = myna-config ]; then
            continue
        fi
        return 0
    done
    return 1
}

# Print a GVariant string array without the given elements, or nothing when
# none of them is there.
without() {
    python3 -c '
import ast, sys
text, drop = sys.argv[1], set(sys.argv[2:])
items = ast.literal_eval(text.removeprefix("@as ").strip())
if drop & set(items):
    kept = [item for item in items if item not in drop]
    print(repr(kept) if kept else "@as []")
' "$@"
}

# Drop the given values from the string-array dconf key `key`, if there.
dconf_drop() {
    local key=$1 current next
    shift
    current=$(dconf read "$key")
    [ -n "$current" ] || return 0
    next=$(without "$current" "$@")
    [ -n "$next" ] || return 0
    run dconf write "$key" "$next"
}

# Print the app ids holding Myna's GlobalShortcuts grants: myna_myna, and any
# other id whose every shortcut is the daemon's `dictate` with a description
# Myna has ever registered. A portal that could not resolve the snap stored
# Myna's grant under ".". Ids only in the applications list count too.
myna_shortcut_apps() {
    dconf dump "$GLOBAL_SHORTCUTS" | python3 -c '
import sys
from gi.repository import GLib
DESCRIPTIONS = {
    "myna dictation (hold to talk)",
    "myna dictation (tap to start/stop)",
    "Dictation (hold to talk)",
    "Dictation (tap to start or stop)",
    "Dictation (press to start and stop)",
}
dump = GLib.KeyFile()
text = sys.stdin.read()
dump.load_from_data(text, len(text.encode()), GLib.KeyFileFlags.NONE)
def value(group, key, kind):
    if not dump.has_group(group) or key not in dump.get_keys(group)[0]:
        return None
    return GLib.Variant.parse(GLib.VariantType(kind), dump.get_value(group, key)).unpack()
apps = set(value("/", "applications", "as") or [])
apps |= {group for group in dump.get_groups()[0] if group != "/"}
for app in sorted(apps):
    shortcuts = value(app, "shortcuts", "a(sa{sv})")
    if app == sys.argv[1] or shortcuts and all(
        sid == "dictate" and props.get("description") in DESCRIPTIONS
        for sid, props in shortcuts
    ):
        print(app)
' "$PORTAL_APP"
}

dconf_reset_dir() {
    [ -n "$(dconf list "$1" 2>/dev/null)" ] || return 0
    run dconf reset -f "$1"
}

remove_path() {
    [ -e "$1" ] || [ -L "$1" ] || return 0
    run rm -rf "$1"
}

mapfile -t snaps < <(snap list 2>/dev/null | awk 'NR > 1 && $1 ~ /^myna(-|$)/ { print $1 }')
# Backends first: removing myna last means its daemon never sees a backend
# disappear while it is still running.
mapfile -t snaps < <(printf '%s\n' "${snaps[@]}" | sort -r | grep . || true)

echo "Purging Myna for $(id -un) on $(hostname)$([ "$DRY" -eq 1 ] && echo ' (dry run)'):"
[ "${#snaps[@]}" -eq 0 ] || echo "  snaps: ${snaps[*]}"
[ "$DEB" -eq 0 ] || echo "  deb: myna-config"
if [ "$DRY" -eq 0 ] && [ "$YES" -eq 0 ]; then
    read -r -p "Continue? [y/N] " answer
    [ "$answer" = y ] || [ "$answer" = Y ] || exit 1
fi

echo "processes:"
pgrep -x myna-config >/dev/null && run pkill -x myna-config

echo "snaps:"
for snap in "${snaps[@]}"; do
    run sudo snap remove --purge "$snap"
done
# Reading the flag needs root too, and unsetting an unset key is a no-op.
run sudo snap unset system experimental.user-daemons
for dir in "$HOME"/snap/myna "$HOME"/snap/myna-*; do
    remove_path "$dir"
done

echo "shortcuts:"
dconf_drop "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings" "$KEYBINDING"
dconf_reset_dir "$KEYBINDING"
mapfile -t portal_apps < <(myna_shortcut_apps)
if [ "${#portal_apps[@]}" -gt 0 ]; then
    dconf_drop "${GLOBAL_SHORTCUTS}applications" "${portal_apps[@]}"
    for app in "${portal_apps[@]}"; do
        dconf_reset_dir "$GLOBAL_SHORTCUTS$app/"
    done
fi

echo "settings:"
dconf_reset_dir /com/canonical/myna/
dconf_reset_dir /org/myna/
dconf_drop "${NOTIFICATIONS}application-children" "$NOTIFICATION_APP"
dconf_reset_dir "${NOTIFICATIONS}application/$NOTIFICATION_APP/"
remove_path "$HOME/.config/myna"

echo "files:"
remove_path "$HOME/.local/share/applications/com.canonical.Myna.Config.desktop"
remove_path "$HOME/.local/share/icons/hicolor/scalable/apps/com.canonical.Myna.Config.svg"
remove_path "$HOME/.local/share/icons/hicolor/scalable/apps/com.canonical.Myna.svg"
# GTK trusts the cache over the directory: a cache still listing the removed
# icons hides every other copy of them.
ICONS="$HOME/.local/share/icons/hicolor"
if [ -f "$ICONS/icon-theme.cache" ]; then
    run gtk-update-icon-cache -f -t -q "$ICONS"
fi
remove_path "$HOME/.local/share/gnome-shell/extensions/$EXTENSION"
if ! packaged_extension; then
    dconf_drop /org/gnome/shell/enabled-extensions "$EXTENSION"
fi
if [ -e "$LEGACY_SCHEMA" ] && ! dpkg -S "$LEGACY_SCHEMA" >/dev/null 2>&1; then
    run sudo rm -f "$LEGACY_SCHEMA"
    run sudo glib-compile-schemas "$SCHEMA_DIR"
fi

if [ "$DEB" -eq 1 ] && dpkg -s myna-config >/dev/null 2>&1; then
    echo "deb:"
    mapfile -t debs < <(dpkg-query -W -f '${db:Status-Abbrev} ${Package}\n' 'myna-config*' 2>/dev/null | awk '$1 == "ii" { print $2 }')
    run sudo apt-get purge -y "${debs[@]}"
fi

echo "done$([ "$DRY" -eq 1 ] && echo ' (dry run, nothing changed)'). Log out and back in before testing."
