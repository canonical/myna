# Feature Specification: Audio Adapter Library

**Feature Branch**: `001-rust-audio-adapter`

**Created**: 2026-07-15

**Status**: Draft

**Input**: User description: "I would like to implement a Rust library for a pipewire / pulseaudio compatible reading of audio buffers, resampling it to a target format, sample rate using native (session manager provided when possible) functionality, optionally deverbing it and overall preparing for speech to text model inference. For ideas and requirements with the rest of the system, see https://github.com/canonical/myna/tree/testbed-and-service-api and /Users/mz2/Downloads/audio-adapter-api.md"

## Clarifications

### Session 2026-07-15

- **Q**: What should the upper bound on the rolling buffer be?  
  **A**: Configurable duration with a default; the default maximum rolling buffer duration is 10 seconds.
- **Q**: Should the adapter support overlapping audio windows, and who controls that setting?  
  **A**: No overlapping windows; the adapter delivers contiguous (non-overlapping) frames, and overlapping windowing is out of scope.
- **Q**: Should the library be session-aware or expose stateless audio primitives?  
  **A**: Stateless audio primitives only; the caller manages the dictation/session lifecycle and uses the library to open and close an audio stream on demand.
- **Q**: When the consumer reads frames too slowly and the bounded buffer fills, what should the library do?  
  **A**: Drop the oldest buffered frames to make room, notify the consumer of the overrun, and apply smoothing at frame boundaries to minimize audible artifacts such as clicks or pops.
- **Q**: How should the library handle frame boundaries when the consumer pulls at irregular intervals?  
  **A**: The library MUST avoid clipping, discontinuities, and other audible artifacts across frame boundaries regardless of when the consumer reads.
- **Q**: Should the library support multiple concurrent open streams?  
  **A**: One open stream per input audio node; opening a stream is an idempotent "ensure open" operation that is a no-op if the node already has an open stream. The library supports any audio-producing node or sink that the underlying audio server exposes (e.g., PipeWire nodes), not only physical microphones.
- **Q**: What device / node enumeration should the library provide?  
  **A**: The library MUST enumerate available input nodes and expose metadata including node identifier, human-readable name/description, and the sample rates/formats the node supports.
- **Q**: When the selected input node is disconnected while its stream is open, what should the library do?  
  **A**: Deliver a device-lost error and close the stream; the caller decides whether to re-open on another node.
- **Q**: When the audio server changes the source format unexpectedly mid-stream, what should the library do?  
  **A**: Renegotiate transparently and keep delivering target-format frames without interruption; report an error only if the new source format cannot be converted to the target.
- **Q**: When the audio server momentarily underruns (delivers no samples for a short period), how should the library represent the gap to the consumer?  
  **A**: Fill the gap with silence so the delivered timeline stays continuous, and notify the consumer that an underrun occurred (the silent span is synthetic). The silence fill itself must not introduce clipping or other audible artifacts at the transitions between real audio and the silent span.
- **Q**: When the sandbox runs with the PulseAudio backend, how should PipeWire-only tests (native node enumeration, session-manager routing) be handled?  
  **A**: Maintain explicitly declared per-backend test subsets; the backend matrix runs each backend's declared subset, and results report against that declaration.
- **Q**: May sandboxed test runs depend on network access?  
  **A**: Initial provisioning may download and cache dependencies; after that, sandbox launches and all test executions must succeed with no network access.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Open an audio stream and deliver frames for STT (Priority: P1)

The dictation client needs a continuous stream of microphone audio to send to the inference backend. The audio adapter library exposes stateless audio-stream primitives: the consumer opens a capture stream from the selected or default input device, reads frames incrementally while the stream is open, and closes the stream when dictation ends.

**Why this priority**: This is the core capability that enables the rest of the dictation pipeline. Without reliable audio capture and streaming, no speech recognition can occur.

**Independent Test**: Initialize the library, open an audio stream, speak into a microphone, and verify that a downstream test consumer receives a continuous sequence of artifact-free audio frames that can be decoded into intelligible speech.

**Acceptance Scenarios**:

1. **Given** a microphone is available and permission is granted, **When** the consumer opens an audio stream, **Then** the library begins delivering audio frames within 100 ms.
2. **Given** an open stream, **When** the consumer closes the stream, **Then** the library stops delivering frames, releases the audio source, and clears in-memory buffers.
3. **Given** no microphone is available, **When** the consumer opens an audio stream, **Then** the library reports an error and does not attempt to deliver frames.

---

### User Story 2 - Resample and format-convert to STT-compatible audio (Priority: P2)

Audio servers may expose microphones at a variety of sample rates, sample formats, and channel counts. The inference backend expects a single, stable input format. The library converts any supported source format into the configured target format so the consumer does not need to handle format negotiation itself.

**Why this priority**: Format negotiation is a common source of integration bugs between desktop audio stacks and inference backends. Centralizing it in the adapter keeps the rest of the system simpler and more robust.

**Independent Test**: Provide a captured or synthetic source stream at a non-target sample rate and channel count, configure a target format, and verify that every delivered frame matches the target format.

**Acceptance Scenarios**:

1. **Given** a microphone source running at 48 kHz stereo, **When** the target is configured to 16 kHz mono, **Then** every delivered audio frame is 16 kHz mono.
2. **Given** a source that reports an unsupported or unknown format, **When** capture is requested, **Then** the library reports a format error instead of producing invalid audio.
3. **Given** a running stream, **When** the audio server changes the source format unexpectedly, **Then** the library renegotiates transparently and continues delivering target-format frames without interruption, reporting an error only if the new source format cannot be converted to the target.

---

### User Story 3 - Optional preprocessing for better transcription quality (Priority: P3)

Recorded speech may contain background noise, room reverberation, or long silent passages that reduce transcription accuracy. The library can apply optional preprocessing—such as noise suppression, voice activity detection, and dereverberation—so the inference backend receives cleaner speech.

**Why this priority**: Preprocessing improves accuracy in real-world environments, but it is secondary to the primary capture and conversion pipeline and may be disabled when latency or resource use is critical.

**Independent Test**: Feed noisy or reverberant audio through the library with preprocessing enabled, and verify that silence/non-speech segments are reduced and that downstream transcription accuracy is at least as good as the unprocessed baseline.

**Acceptance Scenarios**:

1. **Given** preprocessing is enabled, **When** a noisy audio stream is captured, **Then** non-speech audio is attenuated or flagged without introducing audible distortion of speech.
2. **Given** voice activity detection is enabled, **When** the user stops speaking, **Then** the library signals the silence event so the consumer can finalize or chunk utterances appropriately.
3. **Given** preprocessing is disabled, **When** audio is captured, **Then** the library passes the original converted frames through without additional latency.

### Edge Cases

- Node disconnected while the stream is open: the library delivers a device-lost error and closes the stream; the caller decides whether to re-open on another node (see FR-016).
- Microphone permission denied: opening the stream fails with a permission error and no frames are delivered (see FR-012).
- Source sample rate cannot be converted to the target: the library reports a format error instead of producing invalid audio (see FR-012, FR-017).
- Stream closed before any frames have been delivered: closing is safe at any time; the audio source is released and buffers are cleared (see FR-008).
- Capture starts while another application is using the same microphone: desktop audio servers support shared capture; the library opens its own stream and neither preempts nor is preempted by the other application.
- Buffer overrun (slow consumer): when a bounded buffer fills, the library drops the oldest buffered frames, notifies the consumer, and applies smoothing to minimize artifacts (see FR-014).
- Audio-server underrun: when the audio server momentarily delivers no samples, the library fills the gap with silence to keep the delivered timeline continuous and notifies the consumer that an underrun occurred (see FR-018).

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The library MUST capture audio from PipeWire- and PulseAudio-compatible audio servers.
- **FR-002**: The library MUST allow the consumer to discover available input nodes, select a specific input device or any audio-producing node/sink exposed by the audio server when the backend supports it, or fall back to a system-default device.
- **FR-003**: The library MUST support one open stream per selected input node; opening a stream on a node that already has an open stream MUST be a no-op that leaves the existing stream intact.
- **FR-004**: The library MUST resample captured audio to the target sample rate configured by the consumer.
- **FR-005**: The library MUST convert captured audio to the target sample format and channel layout configured by the consumer.
- **FR-006**: The library MUST deliver audio frames incrementally to the consumer while the stream is open.
- **FR-007**: The library MUST keep all intermediate audio data in bounded in-memory buffers; the maximum rolling buffer duration MUST be configurable and defaults to 10 seconds. The library MUST NOT persist captured audio to disk by default.
- **FR-008**: The library MUST release the audio source and flush buffers when the caller closes the stream.
- **FR-009**: The library SHOULD prefer native or session-manager audio routing functionality when the audio server provides it.
- **FR-010**: The library SHOULD support optional audio preprocessing, including at least noise suppression and voice activity detection.
- **FR-011**: The library MAY support optional dereverberation ("deverb") when enabled by the consumer.
- **FR-012**: The library MUST return a typed `Error` variant (`NoDevice`, `PermissionDenied`, or `UnsupportedFormat`) with sufficient context for the consumer to display a meaningful message when the microphone or selected node is unavailable, permission is denied, or format negotiation fails.
- **FR-013**: The library MUST deliver contiguous, non-overlapping audio frames while the stream is open; overlapping windowing is out of scope for this feature.
- **FR-014**: When a bounded buffer reaches capacity because the consumer is not consuming frames quickly enough, the library MUST drop the oldest buffered frames to make room, MUST deliver an explicit overrun notification to the consumer, and MUST apply smoothing at loss boundaries to minimize audible artifacts.
- **FR-015**: The library MUST avoid clipping, discontinuities, and other audible artifacts across frame boundaries regardless of when or how the consumer reads frames.
- **FR-016**: When the selected input node is disconnected or otherwise lost while its stream is open, the library MUST deliver a device-lost error to the consumer and close the stream, releasing its resources; the library MUST NOT switch capture to a different node on its own.
- **FR-017**: When the audio server changes the source format mid-stream, the library MUST renegotiate transparently and continue delivering frames in the configured target format without interruption; it MUST report a format error only if the new source format cannot be converted to the target.
- **FR-018**: When the audio server momentarily underruns, the library MUST fill the missing span with silence so the delivered timeline remains continuous and wall-clock aligned, and MUST deliver an underrun notification so the consumer knows the silent span is synthetic. The silence fill MUST NOT itself introduce clipping or other audible artifacts: transitions between real audio and the silent span MUST be smoothed per FR-015.
- **FR-019**: The feature deliverables MUST include visual documentation maintained in version-controlled text form alongside the feature documentation: an architectural block diagram of the library's components and their relationships, and sequence/flow diagrams covering at least stream open and first-frame delivery, the consumer read loop with overrun handling, underrun silence fill, device loss, and mid-stream format renegotiation.
- **FR-020**: The public API, its documentation, and the test suite MUST demonstrably fit the known primary consumer — the dictation client's Speech Controller (docs/architecture). The documentation MUST identify the exact API surface that consumer uses for its push-to-talk session flow (device selection for settings, session start, incremental streaming reads, event/error handling, session end), and the test suite MUST include a consumer-scenario test that exercises that surface end-to-end in the same call pattern the Speech Controller uses.
- **FR-021**: The test regime MUST include a sandboxed execution mode based on Canonical Workshop (https://ubuntu.com/workshop/docs, per constitution Principle IV): a version-controlled sandbox definition provisions an isolated audio server — PipeWire or PulseAudio, selectable at launch — together with virtual input devices into which test fixtures can be injected, and the full test suite (hermetic, integration, consumer-scenario, performance conformance) runs inside the sandbox unchanged, with results recording which backend was exercised. Tests that apply to only one backend MUST belong to an explicitly declared per-backend test subset; the backend matrix runs each backend's declared subset and reports results against that declaration.
- **FR-022**: Sandboxed test runs MUST NOT access, modify, or depend on the host's audio devices, daemons, or configuration; teardown MUST leave no audio daemons, virtual devices, or test state on the host. The sandbox MUST run non-interactively for automation, and environment/provisioning failures (Workshop missing, virtualization unavailable, backend failed to start) MUST be reported distinctly from test failures. Initial sandbox provisioning MAY download and cache dependencies; once provisioned, sandbox launches and all test executions MUST succeed without network access.

### Key Entities *(include if feature involves data)*

- **Audio Adapter**: The library component that manages capture, format conversion, optional preprocessing, and the lifecycle of an audio stream on behalf of the caller.
- **Audio Frame**: A discrete chunk of audio data in the target format, delivered to the consumer together with timing metadata.
- **Stream Configuration**: The consumer-supplied settings for target sample rate, sample format, channels, input node selection, preprocessing options, and maximum rolling buffer duration.
- **Audio Source**: The microphone or input stream provided by the desktop audio server.
- **Preprocessing Pipeline**: The optional set of audio enhancement stages applied before frames are delivered.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: The library begins delivering audio frames within 100 ms of the consumer opening a stream on the constitution's named reference environments.
- **SC-002**: Resampled audio output matches the configured target sample rate and channel layout, and the total sample error versus a high-quality reference resampling of the same source is below 1% over a reference clip.
- **SC-003**: Audio frames are delivered to the consumer with end-to-end latency no greater than 100 ms behind real time under normal system load.
- **SC-004**: Closing the audio stream releases all audio resources and clears in-memory buffers within 200 ms.
- **SC-005**: When preprocessing is enabled, transcription accuracy on a reference noisy/reverberant corpus is at least as high as the unprocessed baseline.
- **SC-006**: The library produces a usable audio stream for dictation sessions without requiring network connectivity.
- **SC-007**: Inside the Workshop sandbox, each backend selection (PipeWire and PulseAudio) passes 100% of its explicitly declared test subset, and a developer with only Workshop installed reaches a completed sandboxed test run with at most two documented commands.
- **SC-008**: During and after a sandboxed test run, zero changes are observable in the host's audio devices, daemons, or configuration.

## Assumptions

- The consumer configures a target format suitable for the chosen inference backend; the default target represents a common speech-to-text input format (16 kHz sample rate, mono, 16-bit PCM).
- The consumer is responsible for dictation/session lifecycle management and for communicating with the inference backend; the audio adapter focuses only on capture and preparation.
- Preprocessing is optional and may be disabled when latency, power, or resource constraints are more important than audio enhancement.
- A PipeWire- or PulseAudio-compatible audio server is installed on the target system.
- The library runs in the same trusted process context as the dictation client; secure-field detection and user-visible error messages are handled by upstream components.
- Audio data is treated as sensitive and is not persisted to disk by default.
- Installing Canonical Workshop is the only permitted host prerequisite for the sandboxed test mode (constitution Principle IV); "sandboxed" means isolation of the audio stack and test state from the host, not a security boundary against malicious code.
