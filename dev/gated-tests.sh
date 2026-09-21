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
#   dev/gated-tests.sh cargo test --workspace --test pipewire_hw
#   dev/gated-tests.sh cargo llvm-cov --no-report --workspace --test pipewire_hw
#
# Everything is private on purpose. ibus_hw changes the *global* input engine
# for the session it runs in, so a developer running this on their desktop must
# not have their real input method (or audio graph) touched: the PipeWire
# daemon, the D-Bus session bus, and the IBus daemon are all scratch instances
# under a temporary XDG_RUNTIME_DIR / XDG_CONFIG_HOME, torn down on exit.
#
# Standing a service up is this script's whole job, so failing to do it is a
# failure of the run, not a reason to run the suite anyway. A missing binary, a
# daemon that never serves, and a daemon that dies while the command runs all
# exit non-zero here. The gates are exported only once the service behind them
# answers, and a suite that sees its gate set may therefore treat an
# unreachable service as a failure rather than a skip.
#
# Opting out means not running this script: `make test-client-hermetic` runs the
# same binaries with the gates unset, where every gated case skips and says so.
set -uo pipefail

if [ "$#" -eq 0 ]; then
    echo "usage: $0 <command> [args...]" >&2
    exit 2
fi

fail() {
    echo "gated-tests: $*" >&2
    exit 1
}

# Is a service this script started still running? Not `kill -0`: a background
# child that has exited stays a zombie until it is waited on, and `kill -0`
# succeeds on a zombie, so the one case this check exists for - a daemon that
# died mid-run - would read as alive.
alive() {
    local pid=$1
    [ -r "/proc/$pid/status" ] || return 1
    ! grep -q '^State:[[:space:]]*Z' "/proc/$pid/status"
}

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

# The virtual audio graph, written as config drop-ins so the daemons own it
# from startup.
#
# `support.null-audio-sink` is timer-driven rather than clocked by a sound
# card, which is what lets it run on a machine with no audio hardware at all.
# The catch is that the session manager suspends an idle node, and a virtual
# device is idle by definition: nothing is playing to it. A suspended node
# stops its timer and hands out an empty stream forever, which reads exactly
# like a working device that happens to be silent. `node.pause-on-idle=false`
# with `session.suspend-timeout-seconds=0` is what keeps it running, and
# without those two lines every capture here returns a bare WAV header.
#
# The sink is what drives the graph: the named `pw-loopback` sources
# pipewire_hw spawns for its own device-selection tests carry no clock of
# their own, so their playback side has to land on a driving sink.
#
# The mic is a loopback off that sink's monitor, not a second null sink: a
# null sink published as a source has to be `Audio/Source/Virtual`, which
# `map_input_device` discards. A loopback's playback side can be a plain
# `Audio/Source`, the class a real microphone has.
write_virtual_audio_config() {
    mkdir -p "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d"
    cat > "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/10-myna-virtual-audio.conf" <<CONF
context.objects = [
  { factory = adapter
    args = {
      factory.name                    = support.null-audio-sink
      node.name                       = "$VIRTUAL_SPEAKER"
      node.description                = "$VIRTUAL_SPEAKER"
      media.class                     = Audio/Sink
      audio.position                  = [ FL FR ]
      node.pause-on-idle              = false
      session.suspend-timeout-seconds = 0
    }
  }
]

context.modules = [
  { name = libpipewire-module-loopback
    args = {
      capture.props = {
        node.name                       = "$VIRTUAL_MIC.input"
        node.passive                    = true
        target.object                   = "$VIRTUAL_SPEAKER"
        stream.capture.sink             = true
      }
      playback.props = {
        node.name                       = "$VIRTUAL_MIC"
        node.description                = "$VIRTUAL_MIC"
        media.class                     = Audio/Source
        audio.position                  = [ FL FR ]
        node.pause-on-idle              = false
        session.suspend-timeout-seconds = 0
      }
    }
  }
]
CONF

    # Hardware off. The suite builds every source it needs, so a real sound
    # card only makes the graph vary by machine: this is why the suite passed
    # on a developer desktop and skipped in CI, and why that gap went unnoticed
    # for so long. With the monitors disabled a laptop reproduces the runner.
    mkdir -p "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d"
    cat > "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/10-myna-no-hardware.conf" <<'CONF'
wireplumber.profiles = {
  main = {
    monitor.alsa      = disabled
    monitor.v4l2      = disabled
    monitor.libcamera = disabled
    monitor.bluez     = disabled
  }
}
CONF
}

# True once wireplumber has published the virtual mic in the graph.
virtual_mic_in_graph() {
    pw-cli ls Node 2>/dev/null | grep -q "$VIRTUAL_MIC"
}

# Does capture actually deliver? A present node proves nothing: a suspended
# virtual device is still listed, still shows as the default, and hands out an
# empty stream, which is indistinguishable from a working device in a quiet
# room. pipewire_hw asserts on buffers arriving, so the gate is decided the
# same way: record, and require a WAV bigger than its 44-byte header.
#
# Keeping this probe even though the graph above is now built to flow is the
# point: it is what catches the config being silently broken by a pipewire or
# wireplumber upgrade, and it decides whether the gate may be exported at all.
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

    # SCRATCH is exported by the outer phase, which also removes it on exit.
    IBUS_DIR="$SCRATCH/ibus"
    mkdir -p "$IBUS_DIR"
    IBUS_PID=""
    # Invoked via trap, which shellcheck cannot see. Older shellcheck flags the
    # body as unreachable (SC2317), newer flags the function (SC2329).
    # shellcheck disable=SC2317,SC2329
    inner_cleanup() {
        [ -n "$IBUS_PID" ] && kill "$IBUS_PID" 2>/dev/null
        [ -n "$IBUS_PID" ] && wait "$IBUS_PID" 2>/dev/null
        return 0
    }
    trap inner_cleanup EXIT

    # GIO in ibus-daemon would otherwise ask this scratch bus to activate
    # org.gtk.vfs.Daemon; dbus-daemon spawns gvfsd as its own child, and when
    # dbus-run-session tears the bus down the orphan prints "A connection to
    # the bus can't be made" as the run's last line. Nothing here needs a VFS.
    export GIO_USE_VFS=local

    if ! command -v ibus-daemon >/dev/null 2>&1 || ! command -v ibus >/dev/null 2>&1; then
        fail "no ibus-daemon/ibus on PATH; ibus_hw needs a real IBus daemon" \
            "(install ibus, or use the .workshop/desktop SDK: make test-client-gated)"
    fi

    # An explicit address, so clients never go looking for an address file:
    # `--daemonize` forks and the parent exits, and if the child then dies
    # it leaves a file naming a dead PID behind. IbusInjector reports that
    # as "address file(s) present but the daemon looks gone (stale PID N)",
    # which is precisely how this failed in CI.
    #
    # No --xim either. XIM needs an X server, and there is none here.
    export IBUS_ADDRESS="unix:path=$IBUS_DIR/bus"
    ibus-daemon --panel disable --address "$IBUS_ADDRESS" \
        >"$SCRATCH/ibus.log" 2>&1 &
    IBUS_PID=$!

    # Probe the way a client connects, not the way a bystander looks. The
    # previous check pinged org.freedesktop.IBus on the *session bus*,
    # which D-Bus happily answers by activating a fresh service: it
    # reported success while the daemon the tests would reach was already
    # gone. `ibus list-engine` is a real libibus client opening a real
    # connection to IBUS_ADDRESS, so it succeeds only when the daemon the
    # suite is about to use is genuinely serving.
    if ! wait_for 150 ibus list-engine; then
        sed 's/^/gated-tests:   ibus: /' "$SCRATCH/ibus.log" >&2
        fail "IBus daemon never served on $IBUS_ADDRESS" \
            "(started here as: ibus-daemon --panel disable --address ...)"
    fi
    export MYNA_IBUS_TESTS=1
    echo "gated-tests: IBus daemon serving on $IBUS_ADDRESS (MYNA_IBUS_TESTS=1)" >&2

    # The session bus itself is all dbus_hw needs.
    export MYNA_DBUS_TESTS=1

    "$@"
    status=$?

    # A daemon that died mid-run leaves the cases after it asserting against
    # nothing, and some of them are written to tolerate an absent field. The
    # run is only green if the service the gate promised was there throughout.
    if ! alive "$IBUS_PID"; then
        sed 's/^/gated-tests:   ibus: /' "$SCRATCH/ibus.log" >&2
        fail "the IBus daemon died during the run; MYNA_IBUS_TESTS results are void"
    fi
    if ! ibus list-engine >/dev/null 2>&1; then
        fail "the IBus daemon stopped serving during the run;" \
            "MYNA_IBUS_TESTS results are void"
    fi
    exit $status
fi

# ---------------------------------------------------------------------------
# Outer phase: scratch dirs, a private PipeWire graph, then re-exec inside a
# private session bus.
# ---------------------------------------------------------------------------
SCRATCH=$(mktemp -d)
export SCRATCH
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
# Private state too: wireplumber remembers default-device choices across runs,
# and inheriting the caller's would make the graph depend on their desktop.
export XDG_STATE_HOME="$SCRATCH/state"
# The settings store is the keyfile backend everywhere, snap or unpackaged
# (see client/myna-core/src/settings.rs). Scratch config home, so this lands
# in the scratch keyfile. Scoped to this script, never a shell profile: with
# it set, every GLib program in the shell would write there.
export GSETTINGS_BACKEND=keyfile
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_STATE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

# No display: the desktop suites must not reach a real compositor, and IBus
# picks its connection up from the session bus, not from X.
unset WAYLAND_DISPLAY DISPLAY

# PipeWire graph: the daemon plus wireplumber to publish the virtual devices
# the config drop-in declares.
if ! command -v pipewire >/dev/null 2>&1 || ! command -v wireplumber >/dev/null 2>&1; then
    fail "no pipewire/wireplumber on PATH; pipewire_hw needs a real graph" \
        "(install them, or use the .workshop/pipewire SDK: make test-client-gated)"
fi
write_virtual_audio_config
pipewire >/dev/null 2>&1 &
PIPEWIRE_PID=$!
PIDS+=("$PIPEWIRE_PID")
wait_for 100 test -S "$XDG_RUNTIME_DIR/pipewire-0" ||
    fail "the private pipewire daemon never bound $XDG_RUNTIME_DIR/pipewire-0" \
        "(started here as: pipewire, with XDG_RUNTIME_DIR in a scratch dir)"
wireplumber >/dev/null 2>&1 &
WIREPLUMBER_PID=$!
PIDS+=("$WIREPLUMBER_PID")
wait_for 100 virtual_mic_in_graph ||
    fail "wireplumber never published $VIRTUAL_MIC in the private graph" \
        "(started here as: wireplumber, config in $XDG_CONFIG_HOME)"
audio_is_flowing ||
    fail "the private PipeWire graph carries no audio;" \
        "pipewire_hw would assert against silence"
export MYNA_PIPEWIRE_TESTS=1
export MYNA_PIPEWIRE_TARGET="$VIRTUAL_MIC"
echo "gated-tests: PipeWire graph carries audio (MYNA_PIPEWIRE_TESTS=1)" >&2

command -v dbus-run-session >/dev/null 2>&1 ||
    fail "no dbus-run-session on PATH; dbus_hw and ibus_hw need a session bus" \
        "(install dbus-daemon, or use the .workshop/desktop SDK)"

# Run, then hold the graph to the same liveness rule as the inner phase: a
# graph that went away mid-run takes every pipewire_hw assertion with it, and
# wireplumber dying is the masked-session-manager failure the suite exists to
# catch. A function so the script's exit status is this one's, with no
# top-level `exit` (which makes shellcheck read the whole file as unreachable).
run_and_check_graph() {
    dbus-run-session -- "$0" --inner "$@"
    local status=$?
    alive "$PIPEWIRE_PID" ||
        fail "the private pipewire daemon died during the run;" \
            "MYNA_PIPEWIRE_TESTS results are void"
    alive "$WIREPLUMBER_PID" ||
        fail "wireplumber died during the run; MYNA_PIPEWIRE_TESTS results are void"
    return $status
}

run_and_check_graph "$@"
