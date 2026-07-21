#!/usr/bin/env bash
# dev-nested.sh — inner-loop development for the myna-shell extension WITHOUT
# logging out: starts a nested GNOME Shell (own session bus + isolated
# config), enables the extension inside it, and runs the myna-desktop
# publisher on that bus. Edit the extension code, close the nested window
# (or Ctrl-C here), re-run — seconds per iteration, your real session
# untouched.
#
# Prerequisites:
#   1. The bundle visible to the Shell (symlink = edits are live):
#        ln -sfn "$PWD/extensions/myna-shell" \
#          ~/.local/share/gnome-shell/extensions/myna-shell@myna.dev
#   2. An inference backend:  uv run myna-server --adapter whisper --socket /tmp/myna.sock
#   3. A built publisher:     cargo build -p myna-desktop
#
# Usage:  extensions/myna-shell/dev-nested.sh [socket]     (default /tmp/myna.sock)
#
# Toggle a dictation session from ANY terminal (the control socket is a plain
# filesystem socket, no bus needed):
#   client/target/debug/myna-desktop --control /tmp/myna-nested.sock --toggle
#
# Note: text injection targets apps inside the nested session (its own IBus);
# for the real spoken-run acceptance use your normal session (quickstart §5).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SOCKET="${1:-/tmp/myna.sock}"
DESKTOP_BIN="${MYNA_DESKTOP_BIN:-$REPO_ROOT/client/target/debug/myna-desktop}"
CONTROL=/tmp/myna-nested.sock

EXT_HOME="$HOME/.local/share/gnome-shell/extensions/myna-shell@myna.dev"
if [ ! -e "$EXT_HOME" ]; then
    echo "dev-nested: symlinking the bundle into the extensions dir"
    ln -sfn "$REPO_ROOT/extensions/myna-shell" "$EXT_HOME"
elif [ ! -L "$EXT_HOME" ]; then
    echo "dev-nested: WARNING: $EXT_HOME is a COPY, not a symlink —"
    echo "  edits to $REPO_ROOT/extensions/myna-shell won't show up."
    echo "  Consider: rm -rf \"$EXT_HOME\" && re-run this script."
fi

# Outer invocation: re-exec inside a private session bus.
if [ -z "${MYNA_NESTED:-}" ]; then
    export MYNA_NESTED=1 REPO_ROOT SOCKET DESKTOP_BIN CONTROL
    exec dbus-run-session -- "$0" "$SOCKET"
fi

# Inner: isolated config/cache so nothing touches the real session's IBus or
# dconf state.
export XDG_CONFIG_HOME="$(mktemp -d)" XDG_CACHE_HOME="$(mktemp -d)"

ibus-daemon --daemonize --panel disable --xim
# GNOME 48+: plain `--wayland` inside a Wayland session IS the nested mode
# (`--nested` was removed; `--display-server` opts out of nesting).
gnome-shell --wayland &
SHELL_PID=$!
DESKTOP_PID=""
cleanup() {
    [ -n "$DESKTOP_PID" ] && kill "$DESKTOP_PID" 2>/dev/null || true
}
trap cleanup EXIT

# Let the Shell come up, then enable the extension and start the publisher on
# this session bus.
sleep 4
gnome-extensions enable myna-shell@myna.dev || true
"$DESKTOP_BIN" --dbus --socket "$SOCKET" --control "$CONTROL" &
DESKTOP_PID=$!

cat <<EOF

nested Shell running (close its window or Ctrl-C here to stop).
Toggle dictation from any terminal:
  $DESKTOP_BIN --control $CONTROL --toggle

EOF

wait "$SHELL_PID"
