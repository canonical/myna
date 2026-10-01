#!/bin/bash
# After onboarding: is dictation really set up on a VM? Daemon, backend,
# shortcut, and a Start/Stop session through the daemon's D-Bus API.
#   check-dictation.sh IP
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=../vm/lib.sh
source "$HERE/../vm/lib.sh"
H=$1
read -r -d '' REMOTE <<'EOF'
export XDG_RUNTIME_DIR=/run/user/$(id -u)
export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus
D="gdbus call --session -d com.canonical.Myna.Dictation -o /com/canonical/Myna/Dictation"
prop() { $D -m org.freedesktop.DBus.Properties.Get com.canonical.Myna.Dictation "$1" 2>&1; }
echo "flag: $(snap get system experimental.user-daemons 2>&1 || sudo snap get system experimental.user-daemons)"
snap list 2>/dev/null | grep -E '^myna'
snap connections myna 2>/dev/null | grep -E 'backend|Interface'
echo "daemon: $(systemctl --user is-active snap.myna.myna.service)"
for p in State Activation Shortcut StatusMessage; do echo "$p: $(prop $p)"; done
echo "custom keys: $(gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings)"
for k in $(gsettings get org.gnome.settings-daemon.plugins.media-keys custom-keybindings | grep -o "'[^']*'" | tr -d "'"); do
  s=org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:$k
  echo "  $k: $(gsettings get $s name) $(gsettings get $s binding) $(gsettings get $s command)"
done
since=$(date '+%Y-%m-%d %H:%M:%S')
echo "Start: $($D -m com.canonical.Myna.Dictation.Start 2>&1)"
for i in 1 2 3 4; do sleep 1; echo "  t+$i State $(prop State) rms $(prop AudioRms)"; done
echo "Stop: $($D -m com.canonical.Myna.Dictation.Stop 2>&1)"
sleep 3
echo "State after: $(prop State)"
echo "--- daemon journal since Start"
journalctl --user -u snap.myna.myna.service --since "$since" --no-pager -o cat 2>/dev/null | tail -25
echo "--- backend services"
systemctl --user list-units --no-legend 'snap.myna-*' 2>/dev/null; systemctl list-units --no-legend 'snap.myna-*' 2>/dev/null
EOF
vm_ssh "$H" "bash -s" <<<"$REMOTE"
