# Preface

Read this document when adding or changing an inference snap, model/runtime component, engine manifest, hook, or confinement interface.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

Each model family ships as a separate strictly confined snap. It packages `myna-server`, exposes a WebSocket service on a Unix socket through the `provider` content slot (content id `inference-provider`), and carries model weights and optional runtimes as snap components. The server has no microphone access.

All inference snaps share the `modelctl` control vocabulary and engine/runtime/model manifest structure. Differences are allowed when required by a runtime or model, but common mechanics must remain consistent.

The upstream source of truth for `modelctl` behaviour is https://github.com/canonical/inference-snaps-cli

# Important

- Keep the `modelctl` release aligned across inference snaps.
- Every model manifest declares `capabilities: [realtime-transcription]`, the value modelctl reserves for the realtime session API. Set `format` only when the weights are in a modelctl-supported format (`CTranslate2` for faster-whisper); ONNX, `.nemo` and raw safetensors have no value, so leave it unset.
- Expose `$SNAP_COMMON/share/provider` as the `provider` slot with `write`, not `read`: a named-socket `connect()` through the bind mount needs the rw AppArmor rule.
- `myna-server --share-provider` is the only writer of `provider.env` (`SNAP_NAME`, `SNAP_INSTANCE_NAME`, `UNIX_SOCKET`). Every launcher passes it; none passes `modelctl run --share-provider`, whose file lacks `UNIX_SOCKET` and would replace ours.
- Grant `network-bind` for Unix-socket `listen()`, but do not add the `network` plug for runtime downloads.
- Grant `hardware-observe` only to apps and hooks that perform hardware discovery.
- Ensure installation selects a usable engine where a deterministic choice exists.
- Set `ws.unix-socket` to `$SNAP_COMMON/share/provider/myna.sock` in the install hook and unconditionally in post-refresh, since older revisions carry another path in package scope; do not revive the retired `socket.path` key.
- Keep `snap/hooks/` limited to actual snap hooks.
- Every engine directory needs its server entry point and `engine.yaml`.
- Keep streaming as configuration; never hardcode `--streaming` in engine launchers.
- Validate static packaging with `server/tests/test_snap_packaging.py` and `make lint-snaps`.
- Use the spread adapter smoke test for behavior that manifests cannot prove, including sideload connections and streaming propagation.
