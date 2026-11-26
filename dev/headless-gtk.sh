#!/bin/bash
# headless-gtk.sh - run GTK suites on a private display, bus and compositor.
#
# Two backends, because GTK's Wayland and X11 paths differ (window size,
# focus, decoration, input): `--x11` (the default) under Xvfb, and
# `--wayland` under a headless mutter, which is what our flagship target
# actually renders on.
#
# Why a wrapper and not just `xvfb-run`: a Wayland session keeps
# WAYLAND_DISPLAY in the environment, and GTK prefers Wayland over X11, so a
# bare `xvfb-run` run opens real windows on the developer's compositor and
# reads its portal - which is also why such a run disagrees with CI, where
# there is no Wayland at all.
#
#   dev/headless-gtk.sh cargo test -p myna-config --lib -- --test-threads=1
#   dev/headless-gtk.sh --wayland cargo test -p myna-config --lib -- --test-threads=1
#   dev/headless-gtk.sh --wayland cargo test -p myna-config --lib a_step   # one probe
#
# The GTK suites share GTK's one thread, so pass `--test-threads=1` for a
# suite rather than one test. The gates are set here on purpose: a gated
# suite that skips runs nowhere.

set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)

mode=x11
case "${1:-}" in
--wayland) mode=wayland; shift ;;
--x11) shift ;;
esac

[ $# -gt 0 ] || {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2
    exit 2
}

unset WAYLAND_DISPLAY
export GIO_USE_VFS=local
export MYNA_CONFIG_GTK_TESTS=1
export MYNA_CONFIG_TEMPLATE_TESTS=1
export MYNA_HUD_GTK_TESTS=1

if [ "$mode" = x11 ]; then
    export GDK_BACKEND=x11
    exec dbus-run-session -- xvfb-run -a -s "-screen 0 1024x768x24" "$@"
fi

# Wayland: a headless mutter. Its own session bus and short private runtime
# dir, as dev/nested-session.sh requires of anything that may start an
# accessibility bus; that also keeps the compositor's socket off the host's.
export GDK_BACKEND=wayland
exec "$HERE/nested-session.sh" bash -c '
    set -euo pipefail
    mutter --headless --wayland --no-x11 --wayland-display=wayland-gtk \
        --virtual-monitor 1024x768 >/dev/null 2>&1 &
    compositor=$!
    trap "kill $compositor 2>/dev/null || true" EXIT
    for _ in $(seq 1 100); do
        [ -S "$XDG_RUNTIME_DIR/wayland-gtk" ] && break
        sleep 0.1
    done
    [ -S "$XDG_RUNTIME_DIR/wayland-gtk" ] || {
        echo "headless-gtk: mutter never offered $XDG_RUNTIME_DIR/wayland-gtk" >&2
        exit 1
    }
    export WAYLAND_DISPLAY=wayland-gtk
    "$@"
' headless-gtk "$@"
