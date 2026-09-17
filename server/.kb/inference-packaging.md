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
- Snaps with more than one engine re-select in a `connect-plug-hardware-observe` hook: the install hook cannot score without the plug, so a sideload or a store grant arriving by refresh would otherwise stay on cpu. The hook skips while no engine is active (auto-connect precedes the install hook) and never exits non-zero, which would undo the connection.
- Set `ws.unix-socket` to `$SNAP_COMMON/share/provider/myna.sock` in the install hook and unconditionally in post-refresh, since older revisions carry another path in package scope; do not revive the retired `socket.path` key.
- Keep `snap/hooks/` limited to actual snap hooks.
- Every engine directory needs its server entry point and `engine.yaml`.
- Keep streaming as configuration; never hardcode `--streaming` in engine launchers.
- Validate static packaging with `server/tests/test_snap_packaging.py` and `make lint-snaps`.
- Use the spread adapter smoke test for behavior that manifests cannot prove, including sideload connections and streaming propagation.

## GPU engines

- Ship the GPU stack as a runtime component with its own `site-packages`, run by the base `python3` so the base venv's CPU packages stay off the path, and plug `opengl` on the daemon for the device nodes and the host driver's `libcuda.so.1`.
- An ONNX Runtime CUDA session that cannot load its provider runs on the CPU with only a log line. Check the created session's `get_providers()`; `onnxruntime.get_available_providers()` lists what the wheel was built with and always names CUDA.
- `onnxruntime-gpu` needs the CUDA libraries it links against beside it. Pin the `nvidia-*` wheels explicitly: 1.27's `[cuda,cudnn]` extras name `-cu13` packages that are empty placeholders on PyPI.
- Leave ONNX Runtime's CUDA arena at its default power-of-two growth. `kSameAsRequested` fragments on variable input lengths: streaming exhausted a 12 GB card within 30 s.
- Match GPU engines on `vendor-id` only. modelctl reads `vram` and `compute-capability` through `nvidia-smi`, which these snaps do not stage, so either key makes the engine never match.
- Do not drive a CUDA session through IOBinding onto host buffers that are rewritten in place between runs: the device copy is taken at bind time and the outputs come back stale.
