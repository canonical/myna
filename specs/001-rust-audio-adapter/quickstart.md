# Quickstart: Audio Adapter Library validation

**Feature**: `001-rust-audio-adapter`

Runnable scenarios proving the feature works end-to-end. Contract guarantees referenced as G1–G14 are defined in [contracts/audio-adapter-api.md](contracts/audio-adapter-api.md); types in [data-model.md](data-model.md).

## Prerequisites

- Ubuntu 24.04+ (or any Linux with PipeWire ≥ 1.0; PulseAudio systems exercise the fallback backend)
- Rust stable ≥ 1.85 (`rustup default stable`)
- System packages: `sudo apt install libpipewire-0.3-dev libpulse-dev clang pkg-config pipewire-utils pulseaudio-utils`
- A working microphone **or** the virtual node from the "Virtual node" section below (CI path)

## Sandboxed run (Canonical Workshop — recommended; FR-021/FR-022)

With only [Workshop](https://ubuntu.com/workshop/docs) installed on the host, the repo's `workshop.yaml` provisions the toolchain, an isolated audio server, and virtual input devices:

```bash
workshop launch                                   # PipeWire backend (default)
workshop launch --config backend=pulse            # PulseAudio backend selection (exact knob defined by workshop.yaml)
workshop exec -- cargo test --all-features        # hermetic suite inside the sandbox
workshop exec -- env MYNA_AUDIO_IT=1 cargo test --test integration -- --ignored
```

**Expected**: each backend selection passes 100% of its explicitly declared test subset (SC-007) — PipeWire-only tests (native node enumeration, session-manager routing) belong to the PipeWire subset and do not run under PulseAudio; host audio devices/daemons untouched during and after the run (SC-008); after first-launch provisioning, launches and test runs succeed with no network access; environment failures (Workshop missing, backend failed to start) reported distinctly from test failures. The host-based sections below remain valid for machines where the dependencies are installed directly.

## Build and unit tests (hermetic — MockBackend, no audio server needed)

```bash
cargo build -p myna-audio-adapter --all-features
cargo test  -p myna-audio-adapter --all-features
```

**Expected**: all unit tests pass; suites cover conversion (G1), frame continuity (G2), overrun drop+event+smoothing (G3), underrun silence-fill+event (G4), close semantics (G8), pass-through when preprocessing disabled (G12), and the **consumer-scenario test** (`tests/consumer_scenario.rs`, G15/FR-020) that replays the Speech Controller's push-to-talk call pattern — enumerate → open → timed read loop handling `Frame`/`VoiceActivity`/`DeviceLost`/`Overrun` items → close — against both `MockBackend` and (in the integration suite) the virtual node.

## Virtual node setup (for integration tests without hardware)

```bash
pactl load-module module-null-sink sink_name=myna_test sink_properties=device.description=MynaTest
# feed a fixture into the sink; its monitor acts as our capture node
paplay --device=myna_test tests/fixtures/speech_48k_stereo.wav &
```

Tear down with `pactl unload-module module-null-sink`.

## Integration tests (real PipeWire)

```bash
MYNA_AUDIO_IT=1 cargo test -p myna-audio-adapter --test integration -- --ignored
```

**Expected outcomes** (each maps to a spec scenario):

| Test | Validates |
|---|---|
| `enumerates_nodes_with_metadata` | Node list contains the virtual node with id, description, supported formats (FR-002) |
| `captures_target_format_from_48k_stereo` | 48 kHz stereo source → every frame 16 kHz mono S16LE (US2-1, G1) |
| `open_is_idempotent_per_node` | Second `open_stream` on same node returns existing stream (FR-003, G7) |
| `device_lost_closes_stream` | Unload the null-sink module mid-stream → `DeviceLost` event, stream closed (FR-016, G5) |
| `format_change_renegotiates` | Change source format mid-stream → uninterrupted target-format frames (FR-017, G6) |
| `no_device_errors_cleanly` | Open with `ByName("nonexistent")` → `Error::NoDevice` (US1-3) |
| `speech_controller_session_flow` | Full consumer call pattern against the virtual node: enumerate → open → read loop → close, asserting the exact API surface documented in the contract's "Known consumer" section (FR-020, G15) |

## Latency and lifecycle conformance (example binary)

```bash
cargo run -p myna-audio-adapter --example capture_check
```

`capture_check` opens the default node, reads for 5 s, closes, and prints measurements.

**Expected**:
- `first_frame_ms ≤ 100` (SC-001, G9)
- `steady_state_lag_ms ≤ 100` (SC-003, G10)
- `close_ms ≤ 200` (SC-004, G8)
- exit code 0; non-conforming measurements exit non-zero

## Preprocessing validation (feature-gated)

```bash
cargo run -p myna-audio-adapter --features vad,denoise --example preprocess_check -- tests/fixtures/noisy_speech.wav
```

**Expected**: `VoiceActivity{speaking:false}` events at silent spans of the fixture (US3-2, G11); denoised output RMS in non-speech regions lower than input, speech regions intact (US3-1). Transcription-accuracy comparison against the unprocessed baseline (SC-005) is a manual/benchmark step documented in `tasks.md`, not part of this quickstart.

## Privacy check

```bash
strace -f -e trace=openat -o /tmp/audio_trace.log cargo run -p myna-audio-adapter --example capture_check
grep -v -E '(\.so|\.toml|/proc|/sys|/dev|/run|\.cache/pipewire)' /tmp/audio_trace.log | grep -i -E '(\.wav|\.raw|\.pcm)' && echo "FAIL: audio persisted" || echo "OK: no audio written to disk"
```

**Expected**: `OK` — no audio files written (FR-007, G13). Also verify `capture_check` runs with networking disabled (`unshare -n`) to confirm SC-006/G14.
