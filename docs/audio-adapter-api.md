# Audio Adapter ↔ Dictation Service — interface (food for thought)

**Date:** 2026-06-18
**Status:** Draft for discussion — derived from the Python prototype, not final
**Authors:** Claude, with Charles
**For:** the Rust audio-adapter author
**Prototype refs:** `myna/core/audio.py` (`AudioFormat`, `PcmChunk`,
`AudioSource`), `myna/testbed/sources.py` (`MicSource` — live PipeWire capture)

## How to read this

We prototyped the whole dictation path in Python and validated it end to end
(live PipeWire capture → push → transcription). This is the audio-capture
seam, translated to Rust as a **starting point** — the part of the meeting that
was left "API TBD".

Two layers below, split deliberately:

- **Semantics & invariants** (sections 1–2, 5–7) come from the prototype and the
  project's hard rules. Please treat these as the contract — they're *why* the
  design is shaped this way, and breaking them breaks the system.
- **The Rust shape** (the trait/types) is a **sketch**. You own the idioms —
  `Stream` vs channel, native async-trait vs `async-trait`, cancellation token
  vs drop. Push back freely; the goal is to hand you the contract, not dictate
  Rust style.

The adapter lives **in-process in the dictation service** (a library/crate, not
a daemon — audio is the hottest path; a process hop buys nothing here since it's
the same user/trust domain). The only cross-language boundary in the system is
the *service ↔ inference snap* WebSocket, which is already versioned; this
audio seam stays in-process Rust.

## 1. Invariants the API must honor (non-negotiable)

1. **Audio-push:** the client (this adapter, inside the dictation service) owns
   capture and pushes PCM. The STT service never touches the microphone.
2. **Never persist audio.** Stream from the capture stack straight to chunks;
   nothing hits disk. A bounded in-memory buffer only; discard on session end.
3. **The client owns conversion; the adapter never resamples.** The service
   advertises the PCM format(s) it accepts (capabilities discovery); the adapter
   produces **exactly** that format. If a device delivers something else, the
   adapter converts to the negotiated format — it does not hand the service
   off-format audio and expect resampling.
4. **No transcription/audio content logged by default.**

## 2. Core types

Faithful to the prototype (`AudioFormat` default = 16 kHz mono S16LE, the common
denominator across the candidate ASR models):

```rust
/// Raw PCM stream description.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate_hz: u32,   // default 16_000
    pub channels: u8,          // default 1 (mono)
    pub sample_width_bytes: u8 // default 2 = signed 16-bit LE
}

impl AudioFormat {
    pub fn bytes_per_second(&self) -> u32 {
        self.sample_rate_hz * self.channels as u32 * self.sample_width_bytes as u32
    }
}

/// A contiguous slice of raw PCM in `format`.
#[derive(Clone, Debug)]
pub struct PcmChunk {
    pub data: bytes::Bytes,    // cheap-clone, contiguous; PCM bytes only
    pub format: AudioFormat,
}

impl PcmChunk {
    pub fn duration(&self) -> std::time::Duration { /* len / bytes_per_second */ }
}
```

Note on `sample_width_bytes` / encoding: the wire is implicitly S16LE today, but
whether the negotiated format is **int16 or float32** is an open project
question (we call it T33) — every ASR runtime ultimately wants float32 [-1,1],
and each currently does the int16→float32 conversion itself. If we move that
conversion to the client, it lands **here, in the adapter**. So leave room for
an encoding discriminant (e.g. an `enum Encoding { S16Le, F32Le }`) rather than
hard-assuming 16-bit — see §7.

## 3. The source trait (sketch)

The Python contract is tiny: a source exposes its `format` and yields
`PcmChunk`s from an async iterator. The Rust equivalent is a `Stream`:

```rust
use futures::Stream;
use std::pin::Pin;

pub trait AudioSource: Send {
    /// The exact format this source emits — set by the dictation service from
    /// the STT service's advertised capabilities (§7). The source produces
    /// EXACTLY this; it never resamples to something else.
    fn format(&self) -> AudioFormat;

    /// Begin capture. The stream yields chunks until capture ends cleanly
    /// (then `None`) or hits a fatal fault (then one `Err`, then `None`).
    fn capture(self) -> Pin<Box<dyn Stream<Item = Result<PcmChunk, CaptureError>> + Send>>;
}
```

(Whether `capture` consumes `self`, borrows, or returns a `(handle, stream)`
pair is your call — see lifecycle below. A `tokio::sync::mpsc` receiver wrapped
as a `Stream` is one natural implementation; so is `async_stream`.)

## 4. Errors

Capture faults (device disappears mid-session, PipeWire node vanishes, format
genuinely can't be produced) are **stream-level**, surfaced as an `Err` item so
the dictation service can turn it into a terminal session error rather than a
silent stall:

```rust
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("audio device unavailable: {0}")]
    DeviceUnavailable(String),
    #[error("requested format {0:?} cannot be produced")]
    UnsupportedFormat(AudioFormat),
    #[error("capture backend failed: {0}")]
    Backend(String),
}
```

The exact set is yours; what matters is that a fatal capture problem is
*observable as an error*, not an empty stream. (The service maps these onto its
own terminal error vocabulary — there's an open task to stabilize those codes.)

## 5. Lifecycle — and how it maps to a dictation session

Push-to-talk drives this, and the hotkey owns the boundary (we verified the
GNOME portal delivers press **and** release, so hold-to-talk works without VAD):

| Event | Adapter behavior | Why |
|---|---|---|
| **hotkey press** → start | `capture()`; chunks begin flowing | session begins |
| **hotkey release** → graceful stop | stop capturing, drain any buffered chunks, then end the stream (`None`) | the service then signals end-of-audio and the model finalizes — a *clean* finish |
| **cancel / abort** | drop the stream/handle; remaining buffer discarded | user cancelled; the service abandons the session, commits nothing |
| **device fault** | yield `Err(CaptureError)`, then end | becomes a terminal session error |

The graceful-stop signal in the prototype is an explicit `stop()` on `MicSource`
(separate from the stream); in Rust a `CancellationToken`, a `stop()` on a
handle, or simply dropping a guard all work — pick what's clean. The key
semantic: **graceful stop drains then ends; abort just ends.** The service uses
the difference to decide whether to finalize or discard.

## 6. Backpressure & buffering

Capture produces at real-time rate; the consumer (push over a socket) is usually
faster but can stall. Use a **bounded** channel/buffer — that *is* the
"bounded in-memory ring buffer, discarded on session end" invariant. Bounded,
not unbounded: if the consumer stalls, you want backpressure or controlled
drop-oldest, never unbounded memory growth (and never spill to disk). Chunk
size in the prototype is ~100 ms (configurable) — small enough for low latency,
large enough to avoid per-chunk overhead; a reasonable default to start.

## 7. Format ownership & negotiation

The flow, top to bottom:

1. The dictation service queries the STT service's **capabilities** (the set of
   `AudioFormat`s it accepts) before the session.
2. The service picks one and **configures the adapter** with it
   (`AudioSource::format()` reflects this).
3. The adapter opens the device and produces **exactly** that format —
   downmix to mono, resample to the target rate, and (open question, §2) convert
   int16↔float32 if we decide that conversion lives client-side.

So the adapter takes the target `AudioFormat` as construction input; it doesn't
choose it. This keeps the "service rejects off-format audio; client owns
conversion" invariant intact.

## 8. Observation hook (activity indicator)

The prototype has an optional per-chunk callback used to drive a level
meter / activity indicator (it does **not** affect what's sent — pure
observation). Worth keeping: the UI needs "are we hearing anything" feedback.
In Rust this might be a `watch`/broadcast channel of levels, or a callback —
again your call, but flag it as a real requirement, not an afterthought.

## 9. Device & channel selection

The adapter owns device selection. From the IE114 discussion, two things to
support beyond "system default":

- **Node selection by stable name** (PipeWire `node.name` is stable across
  graph changes; `object.serial`/`id` are not). The prototype passes an
  optional target node.
- **Channel selection** on multi-channel interfaces — pro audio devices may put
  the mic on channels 9/10, not 0/1, so allow specifying channel index(es) to
  pick/downmix from, defaulting to 0 (mono) / 0+1 if stereo.

## 10. Open questions for you

- **Async runtime / stream idiom** — `Stream` vs `mpsc` receiver vs
  `async_stream`; `tokio` assumed but your call.
- **Encoding discriminant** — do we bake int16-vs-float32 into `AudioFormat`
  now (§2/§7, our T33), so the conversion can live here? Leaning yes, room for
  an `Encoding` enum.
- **Capture backend** — `pw-record` subprocess (what the prototype uses) vs
  linking `libpipewire`/`pipewire-rs` directly. Subprocess is simplest to start;
  direct binding is lower-latency and avoids a fork.
- **Where the crate boundary sits** — is the adapter a standalone crate the
  dictation service depends on, with this trait as its public API? (We'd suggest
  yes, versioned as crate semver, so it can evolve independently.)

## Appendix — prototype → Rust mapping

| Python (`myna.core` / testbed) | Rust |
|---|---|
| `AudioFormat` (frozen dataclass) | `AudioFormat` (Copy struct) |
| `PcmChunk(data: bytes, format)` | `PcmChunk { data: Bytes, format }` |
| `AudioSource.chunks() -> AsyncIterator[PcmChunk]` | `capture() -> impl Stream<Item = Result<PcmChunk, CaptureError>>` |
| `MicSource.stop()` (graceful drain-then-end) | `CancellationToken` / handle `stop()` / drop guard |
| `on_chunk` callback (level meter) | `watch`/broadcast of levels, or callback |
| `target` node + (future) channel idx | device/channel selection params |
| adapter rejects/ converts to negotiated format | `format()` set by the service; produce exactly it |

The prototype is the executable reference for the semantics — `MicSource` in
`myna/testbed/sources.py` is ~80 lines and shows the whole capture lifecycle if
it's useful to read alongside.
