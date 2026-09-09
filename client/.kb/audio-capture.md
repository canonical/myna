# Preface

Read this document when changing microphone capture, audio buffering, device selection, format negotiation, or capture telemetry.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

The client owns capture and exposes PCM through the `AudioSource` contract in `myna-core`. `myna-audio` implements that contract with native PipeWire behind a `CaptureBackend` seam.

Capture begins on activation, before the inference backend is ready. A bounded in-memory ring retains pre-ready audio and drains it once forwarding starts. The ring never drops old speech: sustained overload becomes an explicit `CaptureError::Overloaded` after buffered audio drains.

The session controller selects a format advertised by the backend. The capture backend produces exactly that format, including downmixing and resampling. The server does not convert audio.

Input devices are identified by stable PipeWire `node.name` values. Live device discovery is separate from capture. DSP such as noise suppression, echo cancellation, filtering, and gain control belongs in the PipeWire graph, not in `myna-audio`.

# Important

- Never persist audio or log audio/transcript content by default.
- Keep buffering bounded, in memory, and scoped to one capture session.
- Surface capture faults; never turn overflow or unsupported formats into silent loss.
- `stop()` means graceful drain and end. Dropping the source is cancellation.
- Telemetry may expose levels, duration, clipping, and counters, but never samples.
- Treat the hotkey as voice activity detection; do not add implicit VAD to capture.
