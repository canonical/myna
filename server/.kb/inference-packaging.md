# Preface

Read this document when adding or changing an inference snap, model/runtime component, engine manifest, hook, or confinement interface.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

Each model family ships as a separate strictly confined snap. It packages `myna-server`, exposes a WebSocket service on a Unix socket through `ubustt-socket`, and carries model weights and optional runtimes as snap components. The server has no microphone access.

All inference snaps share the `modelctl` control vocabulary and engine/runtime/model manifest structure. Differences are allowed when required by a runtime or model, but common mechanics must remain consistent.

The upstream source of truth for `modelctl` behaviour is https://github.com/canonical/inference-snaps-cli

# Important

- Keep the `modelctl` release aligned across inference snaps.
- Expose `ubustt-socket`; grant `network-bind` for Unix-socket `listen()`, but do not add the `network` plug for runtime downloads.
- Grant `hardware-observe` only to apps and hooks that perform hardware discovery.
- Ensure installation selects a usable engine where a deterministic choice exists.
- Configure `ws.unix-socket`; do not revive the retired `socket.path` key.
- Keep `snap/hooks/` limited to actual snap hooks.
- Every engine directory needs its server entry point and `engine.yaml`.
- Keep streaming as configuration; never hardcode `--streaming` in engine launchers.
- Validate static packaging with `server/tests/test_snap_packaging.py` and `make lint-snaps`.
- Use the spread adapter smoke test for behavior that manifests cannot prove, including sideload connections and streaming propagation.
