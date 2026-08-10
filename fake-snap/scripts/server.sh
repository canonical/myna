#!/bin/bash -eu
# Serve the fake adapter on the shared content slot's socket path.
# The client snap reaches it via its `backend` plug bind-mount.
mkdir -p "$SNAP_COMMON/run"
exec /usr/bin/python3 -m myna.server \
  --adapter fake \
  --socket "$SNAP_COMMON/run/ubustt.sock"
