# Audio Adapter ↔ Dictation Service — capture contract (v2)

**Date:** 2026-06-18 · **Revised:** 2026-07-07 (v2)
**Status:** Settled contract (plan T49) — the v1 discussion draft plus the
workstream-D takeover decisions (2026-07-07), baked in. Built as the
`client/myna-audio` crate (T50 skeleton + fake backend, T51 `pw-record`
subprocess backend, T52 native `pipewire-rs` backend).
**Authors:** Claude, with Charles
**Prototype refs:** `myna/core/audio.py` (`AudioFormat`, `PcmChunk`,
`AudioSource`), `myna/testbed/sources.py` (`MicSource` — live PipeWire capture)

## What changed from v1

v1 was "food for thought" for a then-unassigned Rust author. Charles took over
workstream D (2026-07-07); the v1 open questions (§10 then) are now decisions:

- **Capture sits behind an internal `CaptureBackend` trait** (§5): the
  `pw-record` subprocess first (known-working, ported from `MicSource`), the
  native `pipewire-rs` binding later behind the same seam. The *public* API
  never names the backend.
- **The adapter owns the bounded capture buffer** (§6): `capture()` starts
  filling it at hotkey press; the consumer defers draining until the model is
  `ready`. Overflow policy: **never drop captured speech** — the buffer grows to
  hold everything across the cold-load window and any lag, up to a generous
  bound; past that it faults (`CaptureError::Overloaded`) so the user is told
  the service can't keep up. It never silently truncates and never blocks
  capture. (Corrected 2026-07-20 — see §6; the earlier drop-oldest policy
  silently lost the front of long utterances.)
- **No in-crate DSP** (§10): real filtering (noise suppression etc.) is
  PipeWire filter-chain territory, upstream of our capture node. The v1 §8
  observation hook grows into a **stats tap** (RMS / peak / clipping /
  overflow) for the UI. VAD stays out — push-to-talk means the hotkey is the
  VAD.
- **The consumer-facing types live in `myna-core`** (§3): `AudioSource`,
  `CaptureStream`, `CaptureError`, `StopHandle` sit beside
  `AudioFormat`/`PcmChunk`; `myna-orchestrator` re-exports them unchanged, so
  the T41 mocks and runner keep compiling. The adapter crate depends on
  `myna-core` only — never on the orchestrator.
- **Crate boundary:** `client/myna-audio`, a workspace member versioned with the
  workspace. Public surface: `CaptureSource` (the adapter), `CaptureBackend` +
  `CaptureSpec` + `Producer` (the backend seam), `AudioStats` (the tap),
  `ScriptedBackend` (the permanent fake fixture — same philosophy as
  `FakeAdapter`/`FakeBackend`).

Sections 1, 5, 7, 9 carry over from v1 with the ring semantics made concrete.

## 1. Invariants the API must honor (non-negotiable)

1. **Audio-push:** the client (this adapter, inside the dictation service) owns
   capture and pushes PCM. The STT service never touches the microphone.
2. **Never persist audio.** Stream from the capture stack straight to chunks;
   nothing hits disk. A bounded in-memory buffer only; discard on session end.
3. **The client owns conversion; the STT service never resamples.** The service
   advertises the PCM format(s) it accepts (capabilities discovery); the adapter
   produces **exactly** that format. If a device delivers something else, the
   adapter's backend converts to the negotiated format (§7).
4. **No transcription/audio content logged by default.** Stats (§8) are
   levels and counters, never samples.

## 2. Core types

Implemented in `myna-core` (`audio.rs`), mirroring Python `myna.core.audio`:
`AudioFormat { sample_rate_hz, channels, sample_width_bytes }` (default
16 kHz mono S16LE — the common denominator across the candidate ASR models)
and `PcmChunk { data: Bytes, format }` with `duration()`. `Bytes` is cheap to
clone, so a chunk fans out to the stats tap without copying.

**Encoding (T33, still a team discussion):** the wire is implicitly S16LE
today. The recorded position is: keep s16le, add an `encoding` discriminant
(int16 → float32 is a decode-side reinterpretation). We do **not** bake an
`Encoding` enum into `AudioFormat` yet — it would change the serde shape that
is golden-tested against Python. The adapter treats `sample_width_bytes == 2`
as S16LE (the stats tap assumes it, §8); when T33 lands, the discriminant is
added to `AudioFormat` in both languages at once and the backend gains the
int16→float32 conversion duty.

## 3. The consumer contract (`myna_core`)

What the orchestrator sees. This is the v1 §3 trait, now real code — moved
from `myna-orchestrator::audio` into `myna-core` so implementors don't depend
on the orchestrator:

```rust
/// A capture-side fault, surfaced as an `Err` stream item so the dictation
/// service turns it into a terminal session error rather than a silent stall.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("requested format {0:?} cannot be produced")]
    UnsupportedFormat(AudioFormat),
    #[error("capture backend failed: {0}")]
    Backend(String),
    /// The capture buffer hit its bound because the STT service could not keep
    /// up (a persistently sub-realtime tier). Surfaced — never silently
    /// dropped — so the client can tell the user rather than lose speech.
    #[error("audio buffer overflow after {0:.1}s — the transcription service cannot keep up")]
    Overloaded(f64),
}

/// The stream a source yields once capturing: chunks until a clean end
/// (`None`) or a fatal fault (one `Err`, then `None`).
pub type CaptureStream =
    Pin<Box<dyn Stream<Item = Result<PcmChunk, CaptureError>> + Send>>;

pub trait AudioSource: Send {
    /// The exact format this source emits — set by the dictation service from
    /// the STT service's advertised capabilities (§7).
    fn format(&self) -> AudioFormat;

    /// Begin capture, consuming the source.
    fn capture(self: Box<Self>) -> CaptureStream;
}

/// Cheap-clone graceful-stop handle: `stop()` means drain-then-end.
/// (An `AtomicBool`; backends poll it — promptness contract in §5.)
pub struct StopHandle(/* Arc<AtomicBool> */);
```

Rules of engagement (the contract T21's session controller codes against):

- **`capture()` is the hotkey press.** The device opens and the ring (§6)
  starts filling the moment it is called. Call it at press.
- **Polling may be deferred.** The consumer holds off draining the stream
  until the accept-gate opens (`ready`); **nothing is dropped** while it waits
  — the buffer holds all captured audio (up to the §6 overload bound). This is
  how the §6 requirement is met without a two-phase API.
- **Graceful stop (hotkey release):** `stop()` on the handle → the backend
  stops capturing, everything already captured drains through the stream,
  then `None`. The service then signals end-of-audio and the model finalizes.
- **Abort (cancel):** drop the stream. The backend is stopped, the ring is
  discarded, nothing more is delivered.
- **Fault:** exactly one `Err(CaptureError)`, then `None`. Never an empty
  stream masquerading as a clean end.

## 4. The adapter pipeline (`myna-audio::CaptureSource`)

One public type implements `AudioSource` over any backend:

```text
device ──▶ CaptureBackend ──▶ Producer::push(bytes)
                                   │  re-chunk to whole-frame ~100 ms chunks
                                   │  update stats tap (RMS/peak/clip, §8)
                                   ▼
                          bounded buffer (no-drop; overload faults, §6)
                                   ▼
                          CaptureStream  ◀── drained when the consumer chooses
```

```rust
let source = CaptureSource::builder(negotiated_format)
    .ring_depth(Duration::from_secs(300))   // §6 overload bound; pair with T29
    .chunk(Duration::from_millis(100))      // whole frames, prototype default
    .target("alsa_input.usb-...")           // optional node.name (§9)
    .backend(Box::new(PwRecordBackend::new()))  // T51; ScriptedBackend in tests
    .build();
let stats = source.stats();          // watch::Receiver<AudioStats> (§8)
let stop = source.stop_handle();     // graceful stop (§3)
let stream = Box::new(source).capture();   // ← the hotkey press
```

Re-chunking: backends deliver whatever buffer sizes are natural to them
(subprocess reads, PipeWire quantum callbacks); the adapter accumulates and
emits fixed ~100 ms whole-frame `PcmChunk`s. On a clean end the remainder is
flushed as a final short chunk (whole frames only; a trailing partial frame —
possible only if a backend misbehaves — is dropped, not padded).

## 5. The backend seam (`CaptureBackend`)

Internal to the crate's design but public API, so backends can live in
separate modules (and, if ever useful, separate crates):

```rust
/// What to capture. The adapter passes this through from its builder.
pub struct CaptureSpec {
    /// Produce EXACTLY this. The backend owns any conversion (§7).
    pub format: AudioFormat,
    /// PipeWire node to capture from, by stable `node.name`; None = default.
    pub target: Option<String>,
    /// Channel indices to pick/downmix on multi-channel devices (§9).
    /// None = device default. (Honored by the pipewire-rs backend, T52;
    /// the subprocess backend errors on Some — pw-record can't do it.)
    pub channels: Option<Vec<u8>>,
    /// Graceful-stop flag; the backend must observe it within ~250 ms.
    pub stop: StopHandle,
}

/// Where the backend delivers PCM. Push is synchronous and never blocks —
/// callable from a tokio task, a plain thread, or a realtime callback.
/// Overflow is the buffer's problem (grow, then fault Overloaded), not the
/// backend's; the producer never drops.
pub struct Producer(/* ring + stats + re-chunk internals */);
impl Producer {
    /// Deliver raw PCM (any size). Returns false once the consumer is gone
    /// or capture has ended — the backend should stop producing.
    pub fn push(&mut self, data: Bytes) -> bool;
    /// End capture: clean (None) after a graceful stop / device EOF, or
    /// fatal (Some(err)) — becomes the stream's single `Err`.
    pub fn finish(self, fault: Option<CaptureError>);
}

pub trait CaptureBackend: Send {
    /// Open the device and start producing. Must return quickly (spawn a
    /// task/thread for the capture loop); a failure to *open* is the
    /// `Err` here, a failure *during* capture goes through `finish(Some)`.
    fn start(self: Box<Self>, spec: CaptureSpec, producer: Producer)
        -> Result<(), CaptureError>;
}
```

Stop semantics, restated from the backend's side: when `spec.stop` trips, stop
reading the device, `push` anything already read, then `finish(None)`. Dropping
the consumer stream also trips the same flag (abort) — the backend can't tell
the difference and doesn't need to; the adapter discards the ring on abort.

| Backend | Task | Notes |
|---|---|---|
| `ScriptedBackend` | T50 | The permanent fake fixture: scripted silence/bytes/pacing/faults, zero audio deps. Drives every unit test; the "mock audio adapter" for orchestrator work. |
| `PwRecordBackend` | T51 | Port of `MicSource`: `pw-record --raw --rate … --channels … --format s16 [--target …] -`, bounded reads, terminate on stop. pw-record's own graph link does the resample/downmix to the requested rate/channels. |
| `PipeWireBackend` | T52 **(done)** | Native `pipewire-rs`: node selection by stable `node.name` (`PW_KEY_TARGET_OBJECT`), channel pick/downmix (interleaved-S16 select+average to the negotiated layout), graph-side resample/downmix, no fork. Live device enumeration is a sibling API (`InputDevices`, below). **Now the sole live-capture backend** — the `PwRecordBackend` subprocess is retired (feature 002-native-pipewire-backend, FR-016). **Platform note:** a *bogus* `target` falls back to the default source under the default WirePlumber policy (as `pw-record` did), so strict absent-target faulting is a documented limitation, not enforced. |

## 6. The pre-ready ring (buffering, settled)

**Requirement (promoted 2026-07-06; T21 acceptance criterion):** capture
starts at hotkey **press**; only the *push* to the STT service is gated on
`ready`. The model cold-loads in 0.9–5.8 s (measured, T11) and the default
residency policy idle-unloads after 300 s, so a typical dictation starts
cold-ish — capture-on-ready would swallow the first sentence.

How this contract meets it: the ring inside `CaptureSource` **is** the buffer.
`capture()` (= press) starts the backend filling it; the consumer simply
doesn't poll the stream until `ready`, then drains — buffered chunks first,
then live ones. No arm/drain two-phase API needed.

- **Depth** is a builder param (`ring_depth`), denominated in duration and
  converted at the negotiated format. It is now the **overload bound**, not a
  drop window. **Default: 300 s (~9.6 MB at 16 kHz mono S16LE), provisional** —
  far above any tolerated cold load, so normal dictation never trips it; finite
  so a wedged/over-budget service can't grow it without limit. The final default
  is **one decision with T29**.
- **Overflow: never drop — fault instead (corrected 2026-07-20).** The buffer
  grows to hold *all* captured audio across the cold-load window and any lag; it
  never ages out captured speech. Only if it exceeds `ring_depth` — a service
  *persistently* slower than realtime — does it stop, drain what it holds, and
  end the stream with `CaptureError::Overloaded(seconds)`, which the client
  surfaces ("the transcription service cannot keep up with capture"). Rationale
  for the reversal: the original drop-oldest policy silently discarded the
  **front** of the utterance during the pre-ready cold-load window, so a long
  dictation landed as only its last few seconds — an unacceptable silent loss.
  Informing the user (or, later, regressing to a slower batch tier) beats
  smoothing it over. A realtime callback still can't block, so the producer
  simply latches the fault; the already-buffered audio drains first.
- **Discard on session end.** Abort drops the ring's contents on the floor;
  nothing outlives the stream. (Invariant §1.2.)

### Backpressure, sizing & mode (findings 2026-07-15, spec-kit review)

Two distinct ways the drain falls behind the 1×-realtime capture, with opposite
fixes:

- **Kind 1 — bounded backpressure (cold load).** The push is gated until
  `ready`, so audio accumulates for the cold-load window (0.9–5.8 s measured,
  T11) plus ms-scale scheduler jitter. Bounded and predictable — **this is the
  ring's only real sizing driver.** Size it to comfortably exceed the worst-case
  cold load across shipped tiers (one decision with T29). Memory is a
  non-constraint (32 KB/s → 10 s = 320 KB, 60 s ≈ 2 MB), so err generous.
- **Kind 2 — sustained backpressure.** The consumer is *permanently* slower than
  realtime. No finite buffer fixes this (a bigger one only delays overflow and
  balloons latency). It arises **only when streaming a model that can't decode
  faster than realtime (RTF ≥ ~1)** — inherently over budget, or pushed
  sub-realtime by sustained CPU/GPU contention or thermal throttling. The
  response is **never to drop or insert partial audio**: hold up to the bound,
  then fault `Overloaded` and let the client tell the user this tier can't keep
  up (or, later, regress to batch). `AudioStats::dropped` reads **0** always now
  (the buffer never drops); an `Overloaded` fault — not a nonzero `dropped` — is
  the Kind-2 diagnosis.

**Batch vs streaming — the mode that decides whether Kind 2 even exists:**

- **Batch (commit-on-finalize) — the MVP and the safe floor.** During capture
  the server only *accepts/buffers* PCM (realtime-trivial even under load); the
  heavy decode runs *after* key release, when the mic has stopped. A slow model
  becomes post-release *latency*, never audio loss — Kind 2 cannot reach the
  ring, and generous cold-load sizing is unambiguously free. All shipped
  adapters are commit-on-finalize today (T07/T09/T10a); streaming is T08 (not
  started), so Kind 2 is currently unreachable.
- **Streaming (T08).** The server decodes *during* capture, so a cold-load
  backlog must be *decoded* to clear while live audio keeps arriving. On a
  marginal model this creates a **latency tail whose length ≈ the cold-load
  backlog** — a large cold-load buffer is then a liability, not free (bigger
  buffer → longer tail; at RTF ≈ 1 the tail persists the whole session; at
  RTF ≥ 1 it never closes = Kind 2). Streaming must therefore be **RTF-gated per
  tier** (enable only where the testbed shows RTF comfortably < 1); the
  catch-up/tail dynamics are governed by the server's RTF — an inference-backend
  property, not a ring-size one.

**Two "sizes"?** No — the client ring has **one** driver (Kind 1, cold load).
"Model-running-to-process-chunks" buffering is the **inference backend's**
concern (batch accumulation / streaming decode queue + its server-side memory
tradeoffs), behind its API. The resident memory a second size would optimise is
negligible: the ring is ~1000× smaller than the model weights, so the
resident-memory lever is the residency/idle-unload policy (T29), not ring depth.

### Underruns (device gaps) — nothing to do here

A genuine underrun — the capture *source* produces nothing for a span (a device
xrun) — is already handled one layer down: PipeWire's ALSA source pads gaps with
silence and keeps the timeline continuous (`spa_alsa_skip()` in
`spa/plugins/alsa/alsa-pcm.c` memsets a buffer when no frames are available;
`alsa_recover()` accounts the xrun on the clock). By the time audio reaches the
backend the stream is already continuous, so the crate adds **no** underrun
concept, no silence-fill, no `Underrun` event. An *overrun* (Kind 1/2 above) is
the opposite — real audio the consumer didn't take in time; the response is
**hold up to the §6 bound, then fault `Overloaded`** (never drop, never insert
synthetic silence). Because the buffer no longer drops, there is no
drop-induced splice discontinuity in a healthy session at all. Splice-smoothing
would be DSP and stays out either way (§10).

## 7. Format ownership & negotiation

Unchanged from v1, now with the backend named as the conversion point:

1. The dictation service queries the STT service's **capabilities** (the
   `input_formats` it accepts) before the session.
2. It picks one and constructs the adapter with it (`CaptureSource::builder`);
   `AudioSource::format()` reflects it.
3. The **backend** produces exactly that format — downmix, resample, and
   (pending T33, §2) int16→float32 all happen there, using the capture
   stack's own machinery where possible (PipeWire's graph resampler in both
   real backends).

The adapter takes the target format as construction input; it never chooses
it. The negotiation itself belongs to the session controller (T21), where the
source is constructed — the T41 runner deliberately stamps the source's format
instead and relies on server rejection.

## 8. Stats tap (was: observation hook)

The v1 per-chunk callback, grown into a typed, non-optional part of the API —
the UI needs "are we hearing anything" feedback, and the ring makes it
essential: during a cold load the tap is the only evidence capture is live.

```rust
/// Snapshot of capture health. Levels are linear full-scale [0, 1]
/// (UI converts to dBFS as it likes). Assumes S16LE samples (§2/T33).
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioStats {
    pub rms: f32,               // last chunk
    pub peak: f32,              // last chunk
    pub clipped: bool,          // last chunk touched full scale
    pub captured: Duration,     // total captured this session
    pub dropped: Duration,      // total aged out by ring overflow (§6)
}
```

Delivered as `tokio::sync::watch::Receiver<AudioStats>` from
`CaptureSource::stats()` — cheap for a UI to poll or await changes, naturally
conflating (a level meter wants latest-value, not history). Updated **at
capture time** (as chunks enter the ring), not at drain time — so the meter
moves while the push is still gated. Pure observation: it never affects what
is sent, and it carries levels and counters, never samples (invariant §1.4).

## 9. Device & channel selection

The adapter owns device selection, via `CaptureSpec` (§5). From the IE114
discussion, beyond "system default":

- **Node selection by stable name** (PipeWire `node.name` is stable across
  graph changes; `object.serial`/`id` are not).
- **Channel selection** on multi-channel interfaces — pro audio devices may
  put the mic on channels 9/10, not 0/1; `channels` picks/downmixes from
  specified indices. Native backend only (T52); the subprocess backend
  rejects it rather than silently capturing the wrong channels.

Device *enumeration* (listing nodes for a settings UI) is **now implemented**
(T52) as `InputDevices` in `myna-audio` — separate from `AudioSource`, as
anticipated. It is **live**: `InputDevices::new()` starts a registry watch on a
dedicated PipeWire loop thread; `list()` returns the current input sources
(each `{ node_name, label }`) and `watch()` yields a
`tokio::sync::watch::Receiver<Vec<InputDevice>>` that updates as devices
appear/disappear (feature 002-native-pipewire-backend, FR-008/FR-008a). The
stable `node_name` it yields is exactly what feeds `CaptureSpec.target`.
`myna-dictate --list-devices` prints it live. (`DeviceChange` deltas are a
reserved additive type; the watch-of-list covers the current need.)

## 10. Filtering — PipeWire filter-chain, not in-crate

Decision (2026-07-07): the adapter does **no DSP**. Anything that changes
samples — noise suppression, echo cancellation, high-pass, AGC — belongs in
the PipeWire graph, upstream of our capture node, where it is shared with
every other consumer, configured per-user, and maintained by people who do
DSP for a living. The canonical example: an `rnnoise` filter-chain module
(`libpipewire-module-filter-chain`) whose output node is what we set `target`
to. From the adapter's point of view a filtered graph is indistinguishable
from a clean mic — which is exactly the point.

What the crate offers instead is the **stats tap** (§8): observation, not
transformation. If a future need genuinely can't live in the graph, the
re-chunk point in the pipeline (§4) is where a stage would slot in — but no
such stage exists today, and VAD explicitly stays out (the hotkey is the VAD).

## Appendix — v1 open questions → resolutions

| v1 §10 question | Resolution |
|---|---|
| `Stream` vs `mpsc` vs `async_stream` | Public API is a `Stream` (matches T41's trait); internally a custom buffer, because the no-drop-then-fault overload semantics aren't an mpsc semantic. |
| Encoding discriminant now? | Not yet — T33 is a team discussion; room reserved, S16LE assumed, both languages change together (§2). |
| `pw-record` vs `libpipewire` | Both, sequenced: subprocess first (T51), native later (T52), behind `CaptureBackend` (§5). |
| Crate boundary | `client/myna-audio` workspace member; consumer traits in `myna-core`; adapter never depends on the orchestrator. |
| Graceful-stop idiom | `StopHandle` (atomic flag, ~250 ms promptness contract), shared between consumer and backend (§3/§5). |
| Buffering policy | Adapter-owned buffer, **no-drop** + overload fault, 300 s provisional bound paired with T29 (§6). |
