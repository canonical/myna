# Phase 0 Research: Native PipeWire Capture Backend

**Feature**: 002-native-pipewire-backend · **Date**: 2026-07-15

Resolves the unknowns implied by the spec + Technical Context. Each item:
decision, rationale, alternatives considered.

## R1. Rust binding to PipeWire

**Decision**: Use the `pipewire` crate (pipewire-rs, safe bindings over
libpipewire-0.3) rather than raw FFI or continuing with a subprocess.

**Rationale**: pipewire-rs wraps the C API (context, core, registry, streams)
with lifetime-safe Rust types and integrates with the PipeWire main loop.
System `libpipewire-0.3` is present (v1.6.4), which the bindings link against.
It gives us the registry (for device enumeration, R5) and `pw::stream::Stream`
(for capture, R2) — the two things the subprocess couldn't do in-process.

**Alternatives considered**:
- *Keep `pw-record` subprocess*: rejected by the spec (FR-016 retires it) — the
  fork/exec, the pipe, and the "unrequested EOF = device gone" heuristic are
  exactly what the native backend removes.
- *Raw libpipewire FFI via bindgen*: rejected — reimplements what pipewire-rs
  already provides safely; `clang`/bindgen is not installed and would add build
  friction. pipewire-rs vendors its own sys crate.
- *`libspa`/GStreamer*: heavier, wrong abstraction level for a single capture
  node.

## R2. Capture stream model & the non-blocking `push` invariant

**Decision**: Run a dedicated PipeWire main-loop thread that owns a capture
`Stream` (input, `MediaType::Audio`/`MediaCategory::Capture`). In the stream's
`process` callback, dequeue the buffer, copy the PCM bytes out, and call
`Producer::push(Bytes)` — which is synchronous and never blocks (the adapter's
ring is drop-oldest). On `spec.stop` (polled) or consumer-gone (`push` returns
false), quit the main loop and `Producer::finish(None)`; on a stream error,
`finish(Some(CaptureError::…))`.

**Rationale**: This is the exact shape the existing `Producer` API was designed
for ("callable from a tokio task, a plain thread, or a realtime callback" —
`backend.rs`). The PipeWire main loop is not `Send`/tokio-friendly, so it lives
on its own OS thread; the `CaptureBackend::start` contract already says "spawn a
task/thread for the capture loop and return quickly." The stop flag
(`StopHandle`, an atomic) bridges the tokio consumer world and the PipeWire
thread without shared async state.

**Key detail**: `push` must not allocate-and-block or wait on a lock the drain
side holds. Copying the buffer to `Bytes` and handing it to the ring is O(bytes)
and lock-light; the ring's drop-oldest means the callback never stalls even if
the consumer is paused (pre-ready gate). This satisfies the "never block the
capture path" invariant (audio-adapter-api §6, spec FR-014's capture-time stats).

**Alternatives considered**:
- *Async pipewire integration in the tokio reactor*: pipewire-rs's loop is
  callback/GLib-style; bolting it into tokio is more complex than a dedicated
  thread + atomic stop + the existing ring. Rejected for simplicity.
- *Ship PCM over an mpsc to a tokio task that calls push*: unnecessary hop;
  `push` is already sync and non-blocking, so call it straight from the callback.

## R3. Graph-side format conversion (produce EXACTLY the negotiated format)

**Decision**: Request the stream in exactly the negotiated `AudioFormat`
(rate, channels, S16LE) via the stream's `EnumFormat`/`Format` param (SPA
audio-raw pod). PipeWire's graph inserts an `adapter`/resampler so the stream
delivers the requested rate/channels regardless of the device's native format —
the same mechanism `pw-record --rate/--channels` relied on, now expressed
directly in the format param.

**Rationale**: Honors invariant §1.3 / FR-003 (backend owns conversion, consumer
never resamples) using the graph's own resampler — no DSP in our crate (§10).
The negotiated format arrives via `CaptureSpec.format` unchanged.

**Alternatives considered**:
- *Accept device-native format and convert in-crate*: violates the no-in-crate-
  DSP decision and duplicates the graph resampler. Rejected.
- *libspa audioconvert manually*: the graph does this automatically when you
  request a fixed format; manual is redundant.

**Open confirmation for integration (not a blocker)**: whether to pin the
stream format fully or offer a small `EnumFormat` set and read back the
negotiated one. Decision: pin to the negotiated format and treat a
non-negotiable request as `CaptureError::UnsupportedFormat` — matches the
"reject, don't silently mis-produce" posture (FR-007 spirit). Verified on the
virtual-audio graph in the integration suite.

## R4. Node selection by stable `node.name`

**Decision**: Resolve `spec.target` (a stable `node.name`) via the registry to a
target node, and connect the stream to it (target object / `PW_KEY_TARGET_OBJECT`
by node.name, or `node.target`). `None` → the default source (let PipeWire pick).
Absent target at connect time → `CaptureError::DeviceUnavailable(target)`.

**Rationale**: `node.name` is stable across graph changes (audio-adapter-api §9,
spec FR-004/SC-003); `object.serial`/`id` are not. This is the property the spec
requires selection to survive graph renumbering on.

**Alternatives considered**:
- *Select by `object.id`/serial*: unstable across reconnect / graph changes —
  exactly the bug SC-003 guards against. Rejected.
- *Select by human label (`node.description`)*: not unique/stable; that string is
  the display label (R5), not the selector.

## R5. Live device enumeration + change observer

**Decision**: A `devices` module runs (or shares) a PipeWire main loop with a
registry listener. It maintains the current set of input nodes
(`media.class = Audio/Source`, plus sink monitors excluded by default), each as
`{ node_name (stable id), description (label) }`. The API offers: (a) a snapshot
list, and (b) a change stream — `global` (added) and `global_remove` (removed)
registry events map to `DeviceAdded { node_name, label }` /
`DeviceRemoved { node_name }`, delivered on a `tokio::sync` channel
(`broadcast` or `watch`-of-list).

**Rationale**: The registry is the canonical PipeWire enumeration surface and it
is inherently live — `global`/`global_remove` fire as devices come and go, which
is exactly the clarified live requirement (FR-008/FR-008a, US4 scenario 3). A
`watch::Receiver<Vec<InputDevice>>` (latest full list, conflating) fits a
settings chooser; a `broadcast` of add/remove deltas fits an observer. Pick
`watch<Vec<InputDevice>>` as primary (simplest for a UI: latest wins) and expose
deltas only if a consumer needs them — decided in data-model.

**Alternatives considered**:
- *Point-in-time list only*: rejected by the clarification (live required).
- *Poll the registry periodically*: wasteful and laggy; the registry already
  pushes events. Rejected.
- *Reuse `wpctl`/`pw-cli` subprocess to list*: reintroduces the subprocess we're
  removing, and can't stream changes cleanly. Rejected.

## R6. Threading, shutdown, and abort

**Decision**: The capture main loop runs on a dedicated thread created in
`start`; `start` returns immediately after spawning. Shutdown paths: (1) graceful
stop — `spec.stop` trips, the loop quits, drains, `finish(None)`; (2) abort —
consumer drops the stream, the existing `ConsumerGuard` trips the same stop flag
and closes the ring, the loop observes it and quits; (3) fault — stream/error
callback records a `CaptureError`, quits, `finish(Some)`. The stop flag is polled
via a short main-loop timer (≤250 ms) so promptness holds (FR-012/SC-009) even
when no audio is flowing.

**Rationale**: Mirrors the subprocess backend's proven lifecycle (bounded
stop-poll interval) but without a child process. The 250 ms bound is met by a
timer source on the loop rather than a bounded read.

**Alternatives considered**:
- *Signal the loop via its own eventfd/`pw_loop` invoke instead of polling*:
  cleaner, viable as a refinement; the timer-poll is the simple first cut that
  provably meets the 250 ms contract. Note in tasks as an optional improvement.

## R7. Enumeration for tests without hardware (Principle II)

**Decision**: The hermetic suite does **not** touch PipeWire — it stays on
`ScriptedBackend` (unchanged). Native-backend and enumeration behavior is proven
by an env-gated integration suite (`tests/pipewire_hw.rs`, gated on e.g.
`MYNA_PIPEWIRE_TESTS=1`) that stands up a virtual graph with `pw-loopback` /
a null-sink + a known signal, runs the native backend and the enumerator against
it, and asserts capture/selection/enumeration outcomes. The identical suite runs
on real hardware with no code change (only the env gate + device presence
differ).

**Rationale**: Satisfies Principle II precisely — hermetic tests need no audio
server; integration tests need no *physical* hardware (a virtual interface on the
VM suffices) yet run unchanged on hardware. `pw-loopback`/`pw-cli` are present.

**Alternatives considered**:
- *Only test on hardware*: fails Principle II (won't run in CI). Rejected.
- *Mock the registry/stream*: tests the mock; the real bugs are in negotiation
  and lifecycle. Rejected.

## R8. Removing the subprocess backend

**Decision**: Delete `pw_record.rs`, drop the `pub use PwRecordBackend`, and
switch `myna-cli --mic` to `PipeWireBackend`. Do this **last**, after the native
backend passes its integration suite on hardware, so the default capture path is
never broken on `main` (constitution "Staged Delivery": every merge green).

**Rationale**: FR-016 requires removal; staged branches require the replacement
land and prove out before the removal so `main` always has a working `--mic`.

**Alternatives considered**:
- *Remove first, add native after*: leaves `main` without live capture between
  merges — violates the green-main rule. Rejected.
- *Keep as fallback behind a flag*: explicitly overruled by the clarification
  (retire it). Rejected.

## Cross-cutting notes

- **Encoding**: stays S16LE (spec assumption; T33 is a separate discussion). The
  stats tap already assumes S16 (`backend.rs::levels`); no change.
- **No new wire/protocol surface**: this is entirely client-side capture; the
  session/transport contract and the inference backend are untouched.
- **Workshop**: adding the `pipewire` crate pulls a build/runtime dependency on
  libpipewire-0.3 — the Workshop definition must declare it (Principle IV,
  Complexity Tracking). This is a foundational task.
