# Quickstart: Native PipeWire Capture Backend

**Feature**: 002-native-pipewire-backend

Runnable validation that proves the feature end-to-end. Assumes the workspace
builds (`cd rust && cargo build`) and a running `myna-server` for the live path.

## Prerequisites

- Ubuntu with PipeWire running (`pw-cli info` responds).
- libpipewire-0.3 dev headers available (declared in the Workshop definition; see
  the foundational task). Confirm: `pkg-config --modversion libpipewire-0.3`.
- Rust workspace toolchain (`rust-version` 1.75+).
- For the transcription check: a `myna-server` on a socket, e.g.
  `uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock`.

## 1. Hermetic suite stays green (no audio server)

The `ScriptedBackend` behavioral suite must pass unchanged — proof the seam and
adapter core are untouched.

```shell
cd rust
cargo test -p myna-audio
cargo test --workspace       # nothing else regresses
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all existing crate tests pass; the new `PipeWireBackend`/`InputDevices`
unit tests (data-shape/mapping) pass; no network, no device needed.

## 2. Integration suite against a virtual audio graph (VM/CI)

Stand up a virtual source and run the env-gated integration suite — the same code
that runs on hardware.

```shell
# a virtual input node with a known signal (example; the suite may script this)
pw-loopback --capture-props='media.class=Audio/Source node.name=myna-test-src' &

MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-audio --test pipewire_hw
```

Expected outcomes (map to contract rows):
- captures from `myna-test-src` in exactly the negotiated format (C1–C3),
- rejects an absent target / bad channels with one `Err` (C4, C7),
- graceful stop drains, mid-capture removal faults once (C8, C10),
- `AudioStats::dropped == 0` in a healthy run (C13),
- enumeration lists the node and observes its add/remove (E1–E4),
- no subprocess spawned, nothing on disk (C14, E6).

## 3. Live dictation through the native backend

Replace the subprocess path with the native backend end-to-end.

```shell
uv run myna-server --adapter whisper --model base --socket /tmp/ubustt.sock &
cd rust && cargo build --release

# native mic capture (default source):
./target/release/myna-dictate --socket /tmp/ubustt.sock --language en --mic

# a specific device by stable node.name:
./target/release/myna-dictate --socket /tmp/ubustt.sock --language en --mic \
    --target alsa_input.usb-<your-device>
```

Expected: press → "loading model…"/"ready" gate → speak → release → correct
transcript; the captured/dropped readout shows `dropped 0`; `ps`/`pstree` shows
**no** `pw-record` child of the dictate process (SC-002).

## 4. List input devices (live)

```shell
# device listing flag on the CLI (added by this feature):
./target/release/myna-dictate --list-devices
# leave it running and plug/unplug a USB mic — the list updates live (US4-3).
```

Expected: each input device with its stable `node.name` and human-readable label;
appearances/removals reflected without re-running the command.

## 5. Performance watermark check (Principle III)

```shell
# capture-path watermarks on a reference environment; compares to checked-in baseline
MYNA_PIPEWIRE_TESTS=1 cargo test -p myna-audio --test pipewire_hw perf_ -- --nocapture
```

Expected: peak/steady memory, CPU, and stop-latency within declared tolerance of
the baseline; stop honored < 250 ms (SC-009); no regression versus the recorded
capture-path baseline (SC-008).

## Done / acceptance

- [x] Hermetic + workspace suites green (step 1)
- [x] Integration suite green on the VM and on hardware, unchanged (step 2)
- [x] Live dictation correct with no subprocess (step 3)
- [x] Live device listing works and updates (step 4)
- [x] Watermarks within tolerance (step 5)
- [ ] `pw_record.rs` removed; `--mic` uses `PipeWireBackend`; `main` green
  (gated on one spoken-transcript run — the same gate T51 carries)
