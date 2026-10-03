#!/bin/bash
# The adapter smoke proper, on a machine where myna and the adapter snap
# under test are installed (sideloaded by the spread task, from the store by
# the Testflinger nightly). Environment: SNAP_UNDER_TEST, MODELCTL, MODEL.
# Run as root from a scratch directory; leaves batch.out and streaming.out.
set -eux
HERE=$(cd "$(dirname "$0")" && pwd)

# The modelctl surface must answer on a freshly installed snap. Asserted
# here because it silently did not: parakeet and sherpa once shipped
# without a hardware-observe plug, so every one of these commands died
# when hardware detection could not read /sys - which nothing noticed
# until a benchmark run collapsed halfway through. The static counterpart
# is server/tests/test_snap_packaging.py; this is the version that proves
# it on a real install under confinement.
"$MODELCTL" show-machine --format=json > machine.json
grep -q '"cpus"' machine.json
"$MODELCTL" use-engine --auto --assume-yes --no-restart
"$MODELCTL" show-engine --format=json > engine.json
grep -q '"name"' engine.json
# list-models is a table since modelctl v2.0.0-beta.2 (header row, '*' on
# the active model) - match on the stable JSON representation instead.
"$MODELCTL" list-models --format=json | grep -q "\"name\": \"$MODEL\""
"$MODELCTL" use-model "$MODEL" --assume-yes

snap connect myna:backend "$SNAP_UNDER_TEST":provider

# Confinement: the client cannot read the spread project dir, so stage the
# clip where its own plugs reach.
cp "$HERE/fixture.wav" /var/snap/myna/current/fixture.wav

for mode in batch streaming; do
    [ "$mode" = streaming ] && streaming=true || streaming=false
    # Emission mode is a modelctl config key, so the server has to be told;
    # the client's --mode alone cannot conjure partials the server never emits.
    "$MODELCTL" set streaming="$streaming" --assume-yes --no-restart
    snap restart "$SNAP_UNDER_TEST".server

    # The server binds in its own $SNAP_COMMON/share/provider (host-visible);
    # the content share into the myna snap exists only inside myna's mount
    # namespace, so the host-side wait must target the backend's own path.
    for _ in $(seq 1 90); do
        [ -S "/var/snap/$SNAP_UNDER_TEST/common/share/provider/myna.sock" ] && break
        sleep 1
    done
    if ! [ -S "/var/snap/$SNAP_UNDER_TEST/common/share/provider/myna.sock" ]; then
        echo "=== socket missing in $mode mode; daemon state ==="
        snap services "$SNAP_UNDER_TEST".server || true
        snap logs -n 80 "$SNAP_UNDER_TEST".server || true
        echo "=== modelctl surface ==="
        "$MODELCTL" status 2>&1 || true
        sudo "$MODELCTL" get ws.unix-socket 2>&1 || true
        sudo "$MODELCTL" run -- echo run-ok 2>&1 || true
    fi
    test -S "/var/snap/$SNAP_UNDER_TEST/common/share/provider/myna.sock"
    # The resolver finds the socket only through provider.env.
    grep -qx 'UNIX_SOCKET=myna.sock' "/var/snap/$SNAP_UNDER_TEST/common/share/provider/provider.env"

    # The content share itself: assert from inside the myna snap's mount
    # namespace (the path does not exist on the host at all).
    if ! sudo snap run --shell myna.testbed -c 'test -S /var/snap/myna/current/backend/provider/myna.sock'; then
        echo "=== content share not visible inside the myna snap ==="
        snap connections myna || true
        sudo snap run --shell myna.testbed -c 'ls -laR /var/snap/myna/current/backend/ 2>&1' || true
        exit 1
    fi

    # myna.testbed is stdin-triggered (Enter starts the utterance); feed it
    # one Enter and hold the pipe open while the clip plays out.
    ( printf '\n'; sleep 60 ) | \
        myna.testbed --clip /var/snap/myna/current/fixture.wav \
            --language en --mode "$mode" \
            > "$mode.out" 2>&1 || true
    cat "$mode.out"

    # A final line ("✓ ...") carrying real text. Deliberately not asserted
    # verbatim: whisper-tiny output is not stable enough for that, and a
    # smoke test that fails on a one-word difference gets disabled rather
    # than fixed. Length is the honest proxy - 28 s of speech that decodes
    # to a handful of characters means the audio path is broken.
    grep -q '^✓ ' "$mode.out"
    # "✓ (no speech detected)" is the loudest possible wiring regression.
    if grep -q 'no speech detected' "$mode.out"; then
        echo "FAIL: $mode produced no transcript - audio never reached the model"
        exit 1
    fi
    final_len=$(grep '^✓ ' "$mode.out" | head -1 | wc -c)
    test "$final_len" -gt 100 || {
        echo "FAIL: $mode final transcript only $final_len chars for 28s of speech"
        exit 1
    }
done

# The modes must actually differ. Committed segments ("» ...") arrive
# progressively during streaming and never in commit-on-finalize, so this
# is what proves the toggle reaches the adapter rather than being ignored.
grep -q '^   » ' streaming.out
! grep -q '^   » ' batch.out
