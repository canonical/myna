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
# Everything is private, so this is safe to run on a desktop: a scratch
# XDG_RUNTIME_DIR/XDG_DATA_HOME/XDG_CONFIG_HOME, its own session bus, its own
# Wayland display, and the keyfile GSettings backend so it never writes to the
# caller's dconf. It does NOT touch the caller's real session, and does not
# need - or want - the extension installed there.
#
# Teardown is by process group, on EXIT, TERM and INT, and a run that is
# SIGKILLed is reaped by the next one. That is not belt-and-braces: an earlier
# version killed only the `dbus-run-session` wrapper from an EXIT trap alone,
# so every run that timed out orphaned a headless Shell - plus the portal,
# notification server, ibus and calendar server its bus had activated - and
# each one went on burning a few percent of a core indefinitely.
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

# Carries this script's PID so a session can always be traced back to the run
# that owns it, which is what makes reaping a previous run's leftovers safe.
DISPLAY_NAME="myna-visual-check-$$"

# Every headless session this script has ever started, alive, with a dead
# owner. Nothing else can match: the owning PID is in the display name.
stale_sessions() {
    ps -eo pid=,args= 2>/dev/null |
    sed -n 's/^ *\([0-9]\+\) .*--wayland-display myna-visual-check-\([0-9]\+\).*/\1 \2/p' |
    while read -r pid owner; do
        kill -0 "$owner" 2>/dev/null || echo "$pid"
    done
}

# A SIGKILL leaves no chance to clean up, and an orphaned headless Shell keeps
# rendering - forever, at a steady few percent of a core, alongside the portal,
# notification server, ibus and calendar server its private bus activated. Reap
# any that a previous run lost before adding another.
for stale in $(stale_sessions); do
    echo "entrance-visual: reaping orphaned session $stale from an earlier run" >&2
    kill -KILL -- "-$(ps -o pgid= -p "$stale" 2>/dev/null | tr -d ' ')" 2>/dev/null ||
        kill -KILL "$stale" 2>/dev/null
done

SCRATCH=$(mktemp -d)
CLEANED=0
# Invoked by the traps below. Older shellcheck reads the body as unreachable
# (SC2317), newer flags the function (SC2329).
# shellcheck disable=SC2317,SC2329
cleanup() {
    [ "$CLEANED" = 1 ] && return
    CLEANED=1
    # No group to kill means setsid did not do what it says above; fall back to
    # the process itself so a Shell is never left behind either way.
    [ -z "${SESSION_PGID:-}" ] && [ -n "${SHELL_PID:-}" ] &&
        kill -KILL "$SHELL_PID" 2>/dev/null
    # The whole process group, not just what we started. `dbus-run-session`
    # execs the Shell, and the Shell has its private bus activate a portal, a
    # notification server, ibus and a calendar server. Killing the wrapper
    # alone orphans every one of them.
    if [ -n "${SESSION_PGID:-}" ]; then
        kill -TERM -- "-$SESSION_PGID" 2>/dev/null
        for _ in $(seq 30); do
            kill -0 -- "-$SESSION_PGID" 2>/dev/null || break
            sleep 0.1
        done
        kill -KILL -- "-$SESSION_PGID" 2>/dev/null
    fi
    rm -rf "$SCRATCH" 2>/dev/null
}
# EXIT alone is not enough. A SIGTERM - which is exactly what `timeout` sends,
# and what CI sends on cancellation - kills the script without ever running an
# EXIT trap, and that is precisely the run that would leak a Shell.
trap cleanup EXIT
trap 'cleanup; exit 143' TERM
trap 'cleanup; exit 130' INT

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
# `setsid` so the session is its own process group and can be killed whole. It
# is not a group leader here (a script has no job control), so it calls
# setsid() and execs in place, keeping this PID as the new group's ID.
setsid dbus-run-session -- gnome-shell --headless --virtual-monitor 1920x1080 \
    --wayland-display "$DISPLAY_NAME" >"$LOG" 2>&1 &
SHELL_PID=$!
SESSION_PGID=$(ps -o pgid= -p "$SHELL_PID" 2>/dev/null | tr -d ' ')
# Only group-kill a group we actually own. Had setsid forked instead of
# exec'ing, this would still be the script's own group, and killing that would
# take the script with it.
[ "$SESSION_PGID" = "$SHELL_PID" ] || SESSION_PGID=""

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
