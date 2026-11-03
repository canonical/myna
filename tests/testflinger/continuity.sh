#!/bin/bash
# The capture-continuity half of the Testflinger nightly, run as root from a
# checkout on a resolute lab machine: dictations through the store's
# latest/edge daemon from the machine's own microphone, idle and with every
# core oversubscribed, none of which may lose audio. Virtual runners cannot
# answer this: their hosts stop the vCPUs for longer than a PipeWire cycle.
# Resolute, not noble: noble's realtime portal cannot map a snap's pids, so
# capture there never runs realtime. Results land in out/.
#
# The machine is a server image with no seat, so this stands in for what a
# desktop login gives the user: the audio group for the device ACLs logind
# would grant, a polkit rule for the rtkit grant an active session gets, an
# IBus daemon, and an IBus input context of our own as the focused field.
set -eux
HERE=$(cd "$(dirname "$0")" && pwd)
U=${SUDO_USER:-ubuntu}
UID_=$(id -u "$U")
mkdir -p "$HERE/out"
cd "$HERE/out"

as_user() {
    sudo -u "$U" env XDG_RUNTIME_DIR="/run/user/$UID_" \
        DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID_/bus" "$@"
}

DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends \
    pipewire wireplumber ibus xdg-desktop-portal rtkit polkitd python3-gi gir1.2-ibus-1.0
# Not the pipewire group: its rlimits would hand the daemon SCHED_FIFO
# itself, a path no desktop user's daemon takes (they get rtkit's RR 20).
usermod -aG audio "$U"
cat > /etc/polkit-1/rules.d/50-myna-continuity.rules <<EOF
polkit.addRule(function(action, subject) {
    if (action.id.indexOf("org.freedesktop.RealtimeKit1.") == 0 && subject.user == "$U")
        return polkit.Result.YES;
});
EOF
# Without a display the portal's GTK backend dies and the frontend hangs
# waiting for it; with none configured it serves its own portals, Realtime
# among them.
sudo -u "$U" mkdir -p "/home/$U/.config/xdg-desktop-portal"
printf '[preferred]\ndefault=none\n' | sudo -u "$U" tee "/home/$U/.config/xdg-desktop-portal/portals.conf" >/dev/null
loginctl enable-linger "$U"
# The user manager takes the new groups only when it starts again.
systemctl restart "user@$UID_.service"

snap set system experimental.user-daemons=true
snap install --edge myna
snap install --edge myna-whisper+model-tiny
myna-whisper.whisper use-engine --auto --assume-yes --no-restart
myna-whisper.whisper use-model tiny --assume-yes
snap connect myna:backend myna-whisper:provider

# A machine without a microphone captures from snd-aloop instead, still
# clocked by the kernel rather than a hypervisor.
for _ in $(seq 20); do as_user wpctl inspect @DEFAULT_AUDIO_SOURCE@ >/dev/null 2>&1 && break; sleep 1; done
if ! as_user wpctl inspect @DEFAULT_AUDIO_SOURCE@ >/dev/null 2>&1; then
    modprobe snd-aloop
    for _ in $(seq 20); do as_user wpctl inspect @DEFAULT_AUDIO_SOURCE@ >/dev/null 2>&1 && break; sleep 1; done
fi
as_user wpctl inspect @DEFAULT_AUDIO_SOURCE@ | grep -E 'node.name|node.description'

rc=0
as_user python3 -I "$HERE/continuity.py" || rc=$?
journalctl -b --no-pager "_UID=$UID_" _SYSTEMD_USER_UNIT=snap.myna.myna.service > daemon.log
[ "$rc" = 0 ]

journalctl -b --no-pager > journal.log
python3 -I "$HERE/../lib/apparmor_denials.py" journal.log > apparmor.out || { cat apparmor.out; exit 1; }
cat apparmor.out
