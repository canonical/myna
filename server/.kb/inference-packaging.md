# Preface

Read this document when adding or changing an inference snap, model/runtime component, engine manifest, hook, or confinement interface.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

Each model family ships as a separate strictly confined snap. It packages `myna-server`, exposes a WebSocket service on a Unix socket through the `provider` content slot (content id `inference-provider`), and carries model weights and optional runtimes as snap components. The server has no microphone access. Canonical's other inference snaps, LLMs such as gemma4, share that content id, so Myna Settings counts a provider as a model by its known family's snap name (`myna_core::language::ModelFamily`) or a `task: speech-to-text` slot attribute. Only the unpublished fake snap carries `task`: the store's snap schema rejects unknown slot attributes (`Additional properties are not allowed ('task' was unexpected)`).

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
- An install hook's cpu fallback must not depend on the store: whisper's cpu default (tiny) cannot be fetched on a sideload or offline, so the hook seeds modelctl's `cache.active-model` with the first installed model, default first. It reads `engines/` and `models/` itself because `modelctl engine`/`engines` score hardware and `models` needs an active engine, none of which works before the first selection without hardware-observe.
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

## AMD NPU engines

- The Whisper AMD NPU engine uses embeddable Lemonade with FastFlowLM's portable HRX Linux release, not HRX directly: Lemonade owns model routing/lifecycle while FastFlowLM supplies the Whisper graph, tokenizer, preprocessing and NPU kernels that bare HRX does not.
- Pin both the embeddable Lemonade and FastFlowLM release archives by version and sha256, and combine them in one `lemond-flm` runtime component. Pin `FastFlowLM/Whisper-V3-Turbo-NPU2` by commit and stage it under `models/Whisper-V3-Turbo-NPU2` in a separate model component; `FLM_MODEL_PATH` names the parent of `models/`.
- Match XDNA2 with a typeless PCI `1022:17f0` device plus `snap-connections: [accel, process-control]`. Do not use modelctl's `type: npu` for this device until its PCI class matcher includes XDNA's signal-processing-controller class.
- Plug snapd's `accel` interface for `/dev/accel/accel*` and assume snapd 2.76 or newer. Keep the engine experimental while `accel` and Linux HRX are preview technology.
- FLM needs an unlimited `RLIMIT_MEMLOCK` for XDNA userptr buffers. Plug `process-control` on the daemon, require both `accel` and `process-control` in the engine matcher, and have the engine launcher run `ulimit -l unlimited` before starting Lemonade. `process-control` is manual-connect; this mirrors Lemonade Server's FLM snap setup and prevents auto-selection until the permission is present.
- Start embedded `lemond` on fresh random loopback ports (HTTP plus Lemonade's always-on WebSocket listener) with a fresh bearer token and supervise it with `myna-server`; the public contract remains WebSocket-over-Unix. Configure Lemonade with `offline`, `no_fetch_executables`, an explicit random `websocket_port`, and an explicit `flm.npu_bin` path into the runtime component. `network-bind` is sufficient for the private listeners and public Unix listener. Never add `network`; pre-stage every model/backend file.
- Do not bypass Lemonade's `flm validate --json` gate. FastFlowLM 1.0.8 requires Linux NPU firmware 1.1.0.0 or newer even though its direct `serve` command may still run on 1.0 firmware; embedded Lemonade intentionally refuses the unsupported stack before model load.
- FLM has no ASR unload operation. Keep `sleep-idle-seconds=0` for this engine until modelctl can socket-activate the supervised process; `idle-action=exit` on the current simple daemon would stop service permanently.
