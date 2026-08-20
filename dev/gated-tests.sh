#!/bin/bash
# gated-tests.sh - run a command with the env-gated hardware suites enabled.
#
# The suites in client/*/tests/*_hw.rs sit behind MYNA_*_TESTS so the default
# `cargo test` stays hermetic and needs no audio server or desktop session. The
# services they do need are already installed by the Workshop SDKs
# (.workshop/pipewire, .workshop/desktop), so there is no reason for CI to leave
# them off: this script stands the services up, exports the gates, and runs the
# command inside them.
#
#   dev/gated-tests.sh cargo test -p myna-audio --test pipewire_hw
#   dev/gated-tests.sh cargo llvm-cov --no-report -p myna-audio --test pipewire_hw
#
# Everything is private on purpose. ibus_hw changes the *global* input engine
# for the session it runs in, so a developer running this on their desktop must
# not have their real input method (or audio graph) touched: the PipeWire
# daemon, the D-Bus session bus, and the IBus daemon are all scratch instances
# under a temporary XDG_RUNTIME_DIR / XDG_CONFIG_HOME, torn down on exit.
#
# A service that fails to come up leaves its gate unset, so its suite skips
# cleanly (the same no-op it is offline) rather than failing the run. That
# keeps the script safe to use on a machine that has no PipeWire or no IBus.
set -uo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 2
fi

# Poll for a condition, up to `$1` tenths of a second. Services here start in
# well under a second; the generous ceiling is for a loaded CI runner.
wait_for() {
    local tries=$1
    shift
    for _ in $(seq "$tries"); do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    return 1
}

VIRTUAL_MIC=myna-virtual-mic
VIRTUAL_SPEAKER=myna-virtual-speaker

# The virtual audio graph, written as a PipeWire config drop-in so the daemon
# owns it from startup. `support.null-audio-sink` runs on a timer rather than a
# hardware clock, which is what lets it clock a graph with no sound card in it.
#
# The sink is not there to play anything: a `pw-loopback` source carries no
# clock of its own, so unless its other side lands on a driving sink, capture
# from it stalls and myna-audio faults with "no audio is flowing". That applies
# to the virtual mic here and to the named sources pipewire_hw spawns for its
# own device-selection tests.
write_virtual_audio_config() {
    mkdir -p "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d"
    cat > "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/10-myna-virtual-audio.conf" <<CONF
context.objects = [
  { factory = adapter
    args = {
      factory.name     = support.null-audio-sink
      node.name        = "$VIRTUAL_SPEAKER"
      node.description = "$VIRTUAL_SPEAKER"
      media.class      = Audio/Sink
      audio.position   = [ FL FR ]
    }
  }
]
CONF
}

# True once the virtual mic is a node in the graph. A graph query, not
# `wpctl status`: wireplumber files a pw-loopback source under Filters rather
# than Sources, so the friendly listing claims there is no capture device while
# the graph plainly holds one.
virtual_mic_in_graph() {
    pw-cli ls Node 2>/dev/null | grep -q "$VIRTUAL_MIC"
}

# Does capture actually deliver? Not "is there a node in the graph" - a
# pw-loopback source appears in the graph and still stalls when nothing drives
# it, and a null-audio-sink published as a source appears *and* hands out an
# empty stream forever. Both look identical to a listing and neither can pass
# pipewire_hw, which asserts on buffers arriving.
#
# So the gate is decided by recording: a WAV larger than its 44-byte header
# means the graph carries data, and only then is MYNA_PIPEWIRE_TESTS worth
# setting. Where it does not, the suite skips exactly as it does offline
# instead of failing a build over an environment it never got.
audio_is_flowing() {
    local probe="$SCRATCH/probe.wav"
    rm -f "$probe"
    timeout 5 pw-record --rate 16000 --channels 1 --format s16 "$probe" >/dev/null 2>&1
    [ -f "$probe" ] && [ "$(stat -c %s "$probe" 2>/dev/null || echo 0)" -gt 44 ]
}

# ---------------------------------------------------------------------------
# Inner phase: runs re-executed under dbus-run-session, with a session bus.
# ---------------------------------------------------------------------------
if [ "${1:-}" = "--inner" ]; then
    shift

    if command -v ibus-daemon >/dev/null 2>&1; then
        # --panel disable: no GUI panel to attach to in a headless session.
        ibus-daemon --daemonize --panel disable --xim >/dev/null 2>&1
        if wait_for 100 gdbus call --session --dest org.freedesktop.IBus \
            --object-path /org/freedesktop/IBus \
            --method org.freedesktop.DBus.Peer.Ping; then
            export MYNA_IBUS_TESTS=1
            echo "gated-tests: IBus daemon up (MYNA_IBUS_TESTS=1)" >&2
        else
            echo "gated-tests: IBus daemon did not answer; ibus_hw will skip" >&2
        fi
    else
        echo "gated-tests: no ibus-daemon; ibus_hw will skip" >&2
    fi

    # The session bus itself is all dbus_hw needs.
    export MYNA_DBUS_TESTS=1

    "$@"
    exit $?
fi

# ---------------------------------------------------------------------------
# Outer phase: scratch dirs, a private PipeWire graph, then re-exec inside a
# private session bus.
# ---------------------------------------------------------------------------
SCRATCH=$(mktemp -d)
PIDS=()

cleanup() {
    for pid in "${PIDS[@]:-}"; do
        [ -n "$pid" ] && kill "$pid" 2>/dev/null
    done
    for pid in "${PIDS[@]:-}"; do
        [ -n "$pid" ] && wait "$pid" 2>/dev/null
    done
    rm -rf "$SCRATCH"
}
trap cleanup EXIT

export XDG_RUNTIME_DIR="$SCRATCH/run"
export XDG_CONFIG_HOME="$SCRATCH/config"
export XDG_CACHE_HOME="$SCRATCH/cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

# No display: the desktop suites must not reach a real compositor, and IBus
# picks its connection up from the session bus, not from X.
unset WAYLAND_DISPLAY DISPLAY

# PipeWire graph: the daemon, wireplumber to populate it, and a virtual mic fed
# by the driving sink. wireplumber files a pw-loopback source under Filters
# rather than Sources, so `wpctl status` will claim there is no capture device
# even when there is one; the readiness check below records instead of asking.
if command -v pipewire >/dev/null 2>&1 && command -v wireplumber >/dev/null 2>&1; then
    write_virtual_audio_config
    pipewire >/dev/null 2>&1 &
    PIDS+=("$!")
    if wait_for 100 test -S "$XDG_RUNTIME_DIR/pipewire-0"; then
        wireplumber >/dev/null 2>&1 &
        PIDS+=("$!")
        if wait_for 100 pw-cli info 0 && command -v pw-loopback >/dev/null 2>&1; then
            pw-loopback -C "$VIRTUAL_SPEAKER.monitor" \
                --playback-props="media.class=Audio/Source node.name=$VIRTUAL_MIC node.description=$VIRTUAL_MIC" \
                >/dev/null 2>&1 &
            PIDS+=("$!")
            wait_for 100 virtual_mic_in_graph
        fi
        if audio_is_flowing; then
            export MYNA_PIPEWIRE_TESTS=1
            export MYNA_PIPEWIRE_TARGET="$VIRTUAL_MIC"
            echo "gated-tests: PipeWire graph carries audio (MYNA_PIPEWIRE_TESTS=1)" >&2
        else
            echo "gated-tests: no audio flowing in the graph; pipewire_hw will skip" >&2
        fi
    else
        echo "gated-tests: pipewire socket never appeared; pipewire_hw will skip" >&2
    fi
else
    echo "gated-tests: no pipewire/wireplumber; pipewire_hw will skip" >&2
fi

if command -v dbus-run-session >/dev/null 2>&1; then
    dbus-run-session -- "$0" --inner "$@"
else
    echo "gated-tests: no dbus-run-session; ibus_hw and dbus_hw will skip" >&2
    "$@"
fi
