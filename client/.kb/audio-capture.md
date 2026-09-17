# Preface

Read this document when changing microphone capture, audio buffering, device selection, format negotiation, or capture telemetry.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

The client owns capture and exposes PCM through the `AudioSource` contract in `myna-core`. `myna-audio` implements that contract with native PipeWire behind a `CaptureBackend` seam.

Capture begins on activation, before the inference backend is ready. A bounded in-memory ring retains audio the consumer has not yet drained - whether because it is not Ready yet or because it has stopped reading - and drains it once forwarding resumes. The ring never drops old speech: sustained overload becomes an explicit `CaptureError::Overloaded` after buffered audio drains.

`CaptureBackend::start` is a non-blocking seam: it returns immediately, and every eventual outcome (open failure, fault, clean end) reaches the consumer through `Producer::finish` rather than a blocking return value. A `Producer` dropped without finishing faults the stream instead of leaving the consumer waiting forever. `AudioSource::health()` exposes that same lifecycle independently of PCM draining, as a latest-value stream of `CaptureHealth` (`Opening`, `Capturing`, `Faulted(CaptureError)`, `Ended`); `Faulted` and `Ended` are terminal and the stream ends once the backend has released its device. A consumer that defers draining PCM until the backend is ready still sees an open failure, a device fault, or an overload on `health()` at once.

The PipeWire backend connects without `RT_PROCESS`, so `process`, stream state changes, the stop-poll timer and the phase-deadline timer all run on the capture loop thread rather than a realtime one; nothing needs locking to cross threads. Startup and stop are bounded and cancellable through a `Supervisor` tracking phase (discovering, linking, capturing): discovery must get a daemon core-sync answer within 3 s, and the stream must deliver its first non-empty buffer within 3 s of connecting, or capture faults `CaptureError::DeviceUnavailable`. A stop before PipeWire accepts the stream (`Paused`/`Streaming`) faults the same way; a stop after that but before any audio, such as a tap released while the device resumes, is a clean empty capture. Once capturing, a rolling 3 s deadline requires continued delivery, not silence: every non-empty buffer, including silent PCM, pushes the deadline forward, so this is not voice-activity detection, and a source that goes quiet at the PipeWire level (unlinked, suspended, a stuck driver) faults the same way once the deadline passes. Teardown disconnects the stream and drops its listener and timers before the producer is taken and finished, so no callback can observe a half-finished producer.

The session controller selects a format advertised by the backend. The capture backend produces exactly that format, including downmixing and resampling. The server does not convert audio.

Input devices are identified by stable PipeWire `node.name` values. Live device discovery is separate from capture. DSP such as noise suppression, echo cancellation, filtering, and gain control belongs in the PipeWire graph, not in `myna-audio`.

# Important

- Never persist audio or log audio/transcript content by default.
- Keep buffering bounded, in memory, and scoped to one capture session.
- Surface capture faults; never turn overflow or unsupported formats into silent loss.
- `stop()` means graceful drain and end. Dropping the source is cancellation.
- Telemetry may expose levels, duration, clipping, counters and voice-activity marks, but never samples. The stats tap carries a noise floor, a speech level and the capture time voice was last heard (`myna-audio/src/voice.rs`, a port of murmure's adaptive VAD); consumers classify, capture never does.
- Capture never gates, trims or delays audio on voice activity. Ending a session on silence is a desktop session policy (`client/.kb/desktop-integration.md`), never a capture behaviour; hold-to-talk keeps the key as the only authority.
