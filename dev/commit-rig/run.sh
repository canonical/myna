#!/bin/bash
# What reaches a field when Myna's IBus injector commits to it through a real
# compositor: a headless GNOME Shell with its own IBus, isolated from the
# desktop by dev/nested-session.sh and a home of its own. The injector is
# client/myna-desktop/examples/commit_probe.rs (build it first:
# cargo build -p myna-desktop --example commit_probe); the fields are a GTK4
# text view and, where the noosphere snap is installed, a Noosphere pane.
#
# usage: dev/commit-rig/run.sh SCENARIO...   (Python, see drive.py)
#   "sizes(Gtk4, 'g', [1322, 4083, 4084, 9000])"   one commit of each size
#   "burst(Gtk4, 'b', 6, 200, [0, 30])"            6 commits, each gap in ms
#   "stalled(Gtk4, 's', 1, 3000, 0, 150, 12)"      12 commits of 3000 bytes,
#                                                  the Shell stopped 150 ms
#                                                  as each is sent
# Noo in place of Gtk4 for Noosphere. Logs land in dev/commit-rig/out/.
set -eu
RIG=$(cd "$(dirname "$0")" && pwd)
MYNA=$(cd "$RIG/../.." && pwd)
export MYNA
umask 022
if [ "${RIG_INSIDE:-}" != 1 ]; then
    H=$(mktemp -d /tmp/myna-rig.XXXXXX)
    mkdir -p "$H/.config" "$H/.cache" "$H/.local/share" "$H/state"
    for v in $(env | grep -oE '^(SNAP[A-Z_]*|NOOSPHERE[A-Z_]*|IBUS_ADDRESS|GTK_IM_MODULE|QT_IM_MODULE|XMODIFIERS)=' | tr -d =); do unset "$v"; done
    export RIG_INSIDE=1 RIG_HOME="$H" HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_CACHE_HOME="$H/.cache" \
        XDG_DATA_HOME="$H/.local/share" XDG_STATE_HOME="$H/state" GIO_USE_VFS=local GVFS_DISABLE_FUSE=1
    status=0
    setsid "$MYNA/dev/nested-session.sh" bash "$0" "$@" &
    group=$!
    wait "$group" || status=$?
    kill -- "-$group" 2>/dev/null || true
    mkdir -p "$RIG/out"; cp -r "$H"/out/. "$RIG/out/" 2>/dev/null || true
    rm -rf "$H"
    exit $status
fi
OUT=$RIG_HOME/out; mkdir -p "$OUT"; export OUT
gnome-shell --headless --wayland --no-x11 --wayland-display wayland-0 --virtual-monitor 1600x1000 >"$OUT/shell.log" 2>&1 &
shell=$!
daemon=
if [ -x /snap/noosphere/current/bin/noosd ]; then
    /snap/noosphere/current/bin/noosd serve >"$OUT/noosd.log" 2>&1 &
    daemon=$!
fi
trap 'kill $daemon "$shell" 2>/dev/null || true; pkill -P $$ 2>/dev/null || true; wait "$shell" 2>/dev/null || true' EXIT
n=0
until [ -S "$XDG_RUNTIME_DIR/wayland-0" ] && ls "$XDG_CONFIG_HOME"/ibus/bus/* >/dev/null 2>&1; do
    n=$((n+1)); [ $n -le 300 ] || { echo "no shell/ibus: $(tail -5 "$OUT/shell.log")"; exit 1; }
    sleep 0.1
done
export WAYLAND_DISPLAY=wayland-0
python3 "$RIG/drive.py" "$@"
