#!/bin/bash
# entrance-visual.sh - the HUD pill's presentation, checked against a real
# GNOME Shell (feature 004-gnome-shell-indicator).
#
#     test/entrance-visual.sh            (from extensions/myna-shell/)
#
# exits 0 when every guarantee holds, 1 otherwise, 77 when it cannot run.
#
# The sibling `*.test.js` suites are pure logic under plain `gjs`: no stage, no
# frame clock, no Clutter easing. That covers everything hud-logic.js decides
# and nothing hud.js *presents*, which is where the 2026-08-20 flicker lived -
# an `opacity` eased with EASE_OUT_BACK, whose overshoot past 255 wrapped a
# guint8 to 24 and blanked the pill for the back half of its own entrance. The
# whole unit suite passed the entire time it was broken.
#
# So this stands up a headless GNOME Shell on a private bus with a virtual
# monitor, loads a driver (test/visual-driver/) that builds the real HudView
# out of this working tree and samples the pill once per presented frame, and
# reads the verdict back out of the shell's log. It needs no screencast and no
# video decoding: the driver reads the actor's own animated properties, which
# is both what the compositor would rasterize and far less flaky than diffing
# frames.
#
# Everything is private and torn down on exit, so this is safe to run on a
# desktop: a scratch XDG_RUNTIME_DIR/XDG_DATA_HOME/XDG_CONFIG_HOME, its own
# session bus, its own Wayland display, and the keyfile GSettings backend so it
# never writes to the caller's dconf. It does NOT touch the caller's real
# session, and does not need - or want - the extension installed there.
#
# A Shell that cannot run here at all - not installed, or a headless mutter
# with no DRM device to fall back from - means skip, not fail (exit 77),
# matching the way dev/gated-tests.sh leaves a gate unset when its service is
# unavailable. So does a Shell too starved to present enough frames to judge:
# the driver says INCONCLUSIVE rather than guessing, since an animation seen at
# three frames could hide a one-frame blank between them. A Shell that *does*
# run and then fails to report is a real failure: at that point the only new
# thing in the session is this extension.
set -uo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SRC=$(dirname "$HERE")
DRIVER_UUID=myna-shell-visual-driver@myna.dev

if ! command -v gnome-shell >/dev/null 2>&1; then
    echo "entrance-visual: no gnome-shell; skipping" >&2
    exit 77
fi
if ! command -v dbus-run-session >/dev/null 2>&1; then
    echo "entrance-visual: no dbus-run-session; skipping" >&2
    exit 77
fi

# The one source of truth for which Shells this extension supports is its own
# metadata. A Shell outside that range cannot load hud.js at all (St's API
# moved under it), so running there proves nothing: skip, the same as having no
# Shell. The driver is given the same range for the same reason.
SUPPORTED=$(sed -n 's/.*"shell-version": *\[\([^]]*\)\].*/\1/p' "$SRC/metadata.json" |
            tr -d '",' | tr -s ' ' | sed 's/^ //; s/ $//')
RUNNING=$(gnome-shell --version 2>/dev/null | sed -n 's/.*Shell \([0-9]*\).*/\1/p')
if [ -z "$SUPPORTED" ] || [ -z "$RUNNING" ]; then
    echo "entrance-visual: could not read Shell versions; skipping" >&2
    exit 77
fi
case " $SUPPORTED " in
    *" $RUNNING "*) ;;
    *)
        echo "entrance-visual: GNOME Shell $RUNNING is outside the extension's" \
             "supported range ($SUPPORTED); skipping" >&2
        exit 77
        ;;
esac

SCRATCH=$(mktemp -d)
# Invoked by the EXIT trap below. Older shellcheck reads the body as
# unreachable (SC2317), newer flags the function (SC2329).
# shellcheck disable=SC2317,SC2329
cleanup() {
    [ -n "${SHELL_PID:-}" ] && kill "$SHELL_PID" 2>/dev/null
    [ -n "${SHELL_PID:-}" ] && wait "$SHELL_PID" 2>/dev/null
    rm -rf "$SCRATCH" 2>/dev/null
}
trap cleanup EXIT

# A private session, in every sense. XDG_RUNTIME_DIR especially: the caller's
# holds `gnome-shell-disable-extensions`, and a second shell that finds that
# marker starts with every extension disabled - which reads exactly like an
# extension that failed to load.
export XDG_RUNTIME_DIR="$SCRATCH/run"
export XDG_DATA_HOME="$SCRATCH/data"
export XDG_CONFIG_HOME="$SCRATCH/config"
export XDG_CACHE_HOME="$SCRATCH/cache"
export XDG_STATE_HOME="$SCRATCH/state"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CACHE_HOME" "$XDG_STATE_HOME" \
         "$XDG_DATA_HOME/gnome-shell/extensions" \
         "$XDG_CONFIG_HOME/glib-2.0/settings"
chmod 700 "$XDG_RUNTIME_DIR"

# Only the driver is enabled. It constructs the one HudView under test and
# owns its lifecycle; the real extension enabled alongside would build a second
# pill and drive it from a bus nobody is publishing on. The driver loads hud.js
# and stylesheet.css straight out of the working tree, so this tests the
# checkout rather than whatever happens to be installed.
DRIVER_DIR="$XDG_DATA_HOME/gnome-shell/extensions/$DRIVER_UUID"
mkdir -p "$DRIVER_DIR"
cp "$HERE/visual-driver/extension.js" "$DRIVER_DIR/"
# Generated, not committed: the driver has to claim the same Shell versions as
# the extension it drives, and one hand-maintained copy of that list is one too
# many.
cat > "$DRIVER_DIR/metadata.json" <<EOF
{
  "uuid": "$DRIVER_UUID",
  "name": "Myna HUD visual driver (test only)",
  "description": "Drives HudView and samples the pill per presented frame.",
  "shell-version": [$(printf '%s' "$SUPPORTED" | sed 's/\([0-9][0-9]*\)/"\1"/g; s/ /, /g')],
  "version": 1
}
EOF
# By absolute path: resolving it relative to the driver's own module URL would
# go through the symlink above and land outside the tree.
export MYNA_SHELL_SRC="$SRC"

# dconf's writer needs a session bus that outlives this script; the keyfile
# backend needs only a file, and keeps the caller's real settings untouched.
export GSETTINGS_BACKEND=keyfile
cat > "$XDG_CONFIG_HOME/glib-2.0/settings/keyfile" <<EOF
[org/gnome/shell]
disable-user-extensions=false
enabled-extensions=['$DRIVER_UUID']

[org/gnome/desktop/interface]
enable-animations=true
EOF

unset WAYLAND_DISPLAY DISPLAY

LOG="$SCRATCH/shell.log"
dbus-run-session -- gnome-shell --headless --virtual-monitor 1920x1080 \
    --wayland-display myna-visual-check >"$LOG" 2>&1 &
SHELL_PID=$!

# The driver prints `DONE <failures>` when it has finished every scenario.
for _ in $(seq 400); do
    grep -q 'MYNA-VISUAL: DONE' "$LOG" && break
    kill -0 "$SHELL_PID" 2>/dev/null || break
    sleep 0.1
done

if ! grep -q 'MYNA-VISUAL: DONE' "$LOG"; then
    if ! grep -q 'GNOME Shell started' "$LOG"; then
        echo "entrance-visual: GNOME Shell never started here; skipping" >&2
        sed 's/^/entrance-visual:   /' "$LOG" >&2
        exit 77
    fi
    echo "entrance-visual: the Shell started but the driver never reported" >&2
    sed 's/^/entrance-visual:   /' "$LOG" >&2
    exit 1
fi

sed -n 's/^.*MYNA-VISUAL: //p' "$LOG" | grep -v '^DONE'
FAILURES=$(sed -n 's/^.*MYNA-VISUAL: DONE //p' "$LOG" | tail -1)

if grep -q 'MYNA-VISUAL: INCONCLUSIVE' "$LOG" && [ "$FAILURES" = "0" ]; then
    echo "entrance-visual: too few frames presented to judge; skipping" >&2
    exit 77
fi
if [ "$FAILURES" = "0" ]; then
    echo "PASS entrance-visual.sh"
    exit 0
fi
echo "FAIL entrance-visual.sh ($FAILURES failing)"
exit 1
