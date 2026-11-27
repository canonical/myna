#!/bin/bash -eu
# Serve the fake adapter in the provider content share, with its provider.env.
# The client snap reaches it via its `backend` plug bind-mount.
mkdir -p "$SNAP_COMMON/share/provider"
export PYTHONPATH="$SNAP/usr/local/lib/python3.12/dist-packages"
# `snap set myna-fake-backend streaming=true` (hooks/configure): the
# streaming script, for the e2e preedit case.
streaming=()
[ "$(snapctl get streaming)" = true ] && streaming=(--streaming)
# `snap set myna-fake-backend transcript='Bonjour à tous.'`: hear that text
# as one segment, for the suites that dictate another language; `|` splits
# it into segments a second apart ('One.|Two.|Three.').
transcript=$(snapctl get transcript)
exec /usr/bin/python3 -m myna.server \
  --adapter fake \
  --socket "$SNAP_COMMON/share/provider/myna.sock" \
  --share-provider \
  --transcript "$transcript" \
  "${streaming[@]}"
