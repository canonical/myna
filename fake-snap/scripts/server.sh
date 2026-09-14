#!/bin/bash -eu
# Serve the fake adapter in the provider content share, with its provider.env.
# The client snap reaches it via its `backend` plug bind-mount.
mkdir -p "$SNAP_COMMON/share/provider"
export PYTHONPATH="$SNAP/usr/local/lib/python3.12/dist-packages"
exec /usr/bin/python3 -m myna.server \
  --adapter fake \
  --socket "$SNAP_COMMON/share/provider/myna.sock" \
  --share-provider
