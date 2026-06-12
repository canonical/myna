# Ubuntu Desktop STT Integration

* **Date:** 2026-06-03
* **Status:** Proposed
* **Authors:** Jean-Baptiste Lallement

## Abstract

This specification describes the integration of offline speech-to-text dictation into Ubuntu Desktop.

The feature allows the user to press and hold a configurable hotkey, speak naturally, and have the recognized text inserted into the currently focused text input surface. Dictation stops when the user releases the hotkey.

The initial target environment is Ubuntu Desktop on Wayland, with GNOME as the primary validated desktop environment. The architecture must remain portable enough to support other desktop environments and, in the future, non-graphical or text-only environments, although those are out of scope for the first iteration.

The solution must work offline, use local inference through the Canonical Inference Snap, and avoid persisting audio by default.

## Rationale

Speech-to-text (STT) is an important accessibility and productivity capability for Ubuntu Desktop. It allows users to enter text using their voice, reduces reliance on keyboard input, and improves usability across a wide range of workflows and abilities.

This project introduces a local, privacy-preserving dictation service that integrates naturally with the Ubuntu Desktop experience. Users should be able to dictate text into applications with the same simplicity and responsiveness they expect from traditional input methods.

The design prioritizes explicit user control, low-latency interaction, and compatibility with modern desktop security models. Speech recognition is performed locally through the Canonical Inference Snap, avoiding dependence on cloud services and allowing dictation to continue without network connectivity.

While the first implementation targets Ubuntu Desktop on Wayland, the architecture is designed to remain portable across desktop environments and input technologies, enabling future evolution without being tied to a specific text input framework.

### **Goals**

The first iteration delivers a push-to-talk dictation experience for Ubuntu Desktop.

Users should be able to press and hold a configurable hotkey, speak naturally, and have recognized text inserted into the currently focused application. Dictation begins immediately when the hotkey is pressed and ends when it is released.

The system must:

* Perform speech recognition locally using the Canonical Inference Snap.  
* Function without network connectivity once the required models are installed.  
* Stream audio to the inference backend in real time, while exposing only stable committed text to the user.  
* In the initial version, show a visible activity indicator during recording/transcription and insert only committed text into the target application.  
* Use only bounded in-memory buffering and avoid persisting audio by default.  
* Support configurable dictation languages, defaulting to the user's display language when available.  
* Operate safely within the Wayland security model and avoid inserting text into unintended targets.  
* Provide an architecture that can support multiple text input backends in future implementations.

### **Non-goals**

The following features are out of scope:

* Wake-word activation and background listening.  
* Continuous dictation sessions.  
* Cloud-based speech recognition services.
* No cloud post-processing.
* Voice assistants, voice commands, or desktop control. This will be handled by separate context-aware features.
* Speech translation.  
* Speaker identification or diarization.  
* Automatic language detection.  
* Custom vocabulary or language model training.  
* Dictation history and audio retention.  
* Advanced correction and editing workflows.  
* Text-only, server-side, or headless deployment scenarios.  
* Guaranteed compatibility with every application toolkit or custom text widget.

Dictation into password fields, authentication prompts, and other secure input surfaces is explicitly unsupported and must be blocked where detection is possible.

## User Experience

### Basic flow

The primary user experience is push-to-talk dictation.

1. The user focuses a text field.  
2. The user presses and holds the configured dictation hotkey.  
3. The system starts a dictation session.  
4. The user speaks.  
5. The system displays an activity indicator showing that recording and transcription are in progress.  
6. Committed text is inserted into the focused application when stable output is available.  
7. The user releases the hotkey.  
8. The system finalizes the current utterance.  
9. The microphone capture stops.

### Target selection

The text input target is selected when the user presses the hotkey.

If focus changes during an active dictation session, the system must not silently commit dictated text into an unrelated application. The implementation should either continue targeting the original text surface or cancel/finalize the session safely, depending on what the text input backend can guarantee.

### Transcription feedback and committed text

For the initial version, the user-facing text model is commit-only.

The Speech Orchestrator may receive unstable or partial hypotheses from the Inference Snap, but those hypotheses are not shown in the target application UI in the first iteration.

Instead, while recording and inference are active, the user sees a clear activity indicator managed by the Text Injection Layer.

Only committed stable text is inserted into the focused application. Once committed, text should no longer be modified, except by explicit correction features in future iterations.

Future versions may introduce provisional/preedit text rendering where replacement safety is guaranteed by the backend.

#### **Post-processing**

The Post-Processing component transforms raw transcription from the Inference Snap into final committed text. It handles:

* Text normalization e.g. convert spoken forms to written forms where appropriate (twenty two → 22)  
* Punctuation insertion  
* Capitalization e.g. sentence starts, proper nouns where possible  
* Formatting e.g. newlines if possible, abbreviations  
* Commands for advanced text manipulation  
* Application-aware transfer (context-specific transformations)  
* Optional: Advanced transformations using a Local LLM for complex cases

Post-processing results are passed to the Text Injection Layer for final commit.

### End of session

When the user releases the hotkey, the system must:

* Stop accepting new audio input.  
* Finalize any buffered speech.  
* Commit the final stable text.  
* Clear the activity indicator.  
* Release the microphone.  
* Discard the in-memory audio buffer.

### Error states

The user must receive clear feedback when dictation cannot start or continue, for example:

* No microphone is available.  
* Microphone permission is denied.  
* The speech model is not installed.  
* The selected language is unsupported.  
* The inference backend is unavailable.  
* No compatible text input target is focused.  
* Dictation is blocked in a secure field.

## Session Lifecycle

The session lifecycle is managed by the Speech Orchestrator and includes the following states:

### Default State transitions

Idle → Starting → Recording → Transcribing → Finalizing → Completed → Idle

### Error transitions

Any state → Error → Idle

### Cancellation transitions

Any active state → Cancelled → Idle

### State descriptions

| State | Meaning |
| --- | --- |
| Idle | No microphone capture |
| Starting | Initializing audio and inference |
| Recording | Audio capture active |
| Transcribing | Inference active, partial hypotheses possible |
| Finalizing | No new audio accepted, inference finishing |
| Completed | Text committed |
| Cancelled | Session aborted |
| Error | Unrecoverable failure |

## Privacy and Security

The system must be explicit and privacy-preserving by design.

### Activation

The microphone must only be captured during an active user-initiated dictation session. The default activation model is press-and-hold.

The system must not continuously listen in the background.

### Audio persistence

Audio must not be written to disk by default.

The implementation may use a bounded in-memory rolling buffer for speech recognition, voice activity detection, overlap, and context. This buffer must be discarded when the dictation session ends.

Diagnostic logging must not include raw audio or full transcription content by default.

### Secure input fields

Dictation must be blocked in password fields and secure prompts where such fields can be detected.

The specification should document residual risks for applications or toolkits that do not expose secure input state reliably.

### Text injection safety

The system must avoid inserting dictated text into the wrong application. The system should also disallow the injection of potentially unsafe key combinations, e.g. TAB, ALT+TAB, function keys, SUPER, …

The target surface should be captured at session start and must not unexpectedly change during a session.

#### Default behaviour

In the default configuration, the system should continue to target the original text surface even if focus changes during an active session. This is the safest option to avoid wrong-target insertion.

Example 1

1. User focuses a text editor in LibreOffice Writer.
2. User presses the dictation hotkey and starts speaking
3. The target remains LibreOffice Writer for the entire dictation session. Everything goes into the text editor, even if the user alt-tabs to another application while still holding the hotkey.

#### Target disappears

If the originally targeted surface disappears during an active session, for instance they close the application or the window is minimized, the system should cancel the session safely, discarding any uncommitted text and showing an appropriate notification.

No attempt should be made to recover or retarget a new surface in this case.

#### Focus changes

If the user changes focus to another application while still holding the hotkey, the system should continue targeting the original surface. There should be no dynamic retargeting to the new surface until the next session.

#### Protected fields

The system must block dictation in secure input fields where secure-input metadata is available. Applications that do not expose secure-input state cannot be protected reliably. Protection is therefore best-effort rather than guaranteed.

Special care is required for:

* Password fields.  
* Lock screen.  
* Polkit prompts.  
* Terminal emulators.  
* Browser address bars.  
* Applications using custom text widgets.

No dictation should be allowed in these contexts where secure input is expected, and the user should receive clear feedback if they attempt to start dictation in such a context.

## Architecture

The system is organized into several interconnected components that handle session management, audio capture, inference, post-processing, and text output. These components work together to provide a responsive, real-time push-to-talk dictation experience.

![image](Myna%20-%20System%20Architecture.png)

### Inference Snap

The Inference Snap is a confined snap providing speech recognition models and runtime.

It is responsible for:

* Hosting pre-trained STT models in multiple sizes (lightweight, default, quality).  
* Providing model runtimes for various accelerators (NVIDIA GPU, Intel NPU, CPU).  
* Exposing a Configuration API for capabilities discovery and model negotiation.  
* Providing a Transcription API (Mistral/OpenAI-like) that accepts raw audio frames and returns text segments.  
* Managing model lifecycle and resource allocation.  
* Versioning APIs and model runtimes to ensure compatibility with clients.

### Speech Orchestrator

The Speech Orchestrator owns the session lifecycle and coordinates all other components.

It is responsible for:

* Global hotkey handling and activation.  
* Starting and stopping dictation sessions.  
* Tracking the current text input target.  
* Managing session state and state transitions.  
* Coordinating audio capture, inference, and text output.  
* Handling cancellation, errors, and focus changes.  
* Enforcing privacy and security rules.  
* Communicating with the Inference Snap through Configuration and Transcription APIs.  
* Streaming audio frames to the inference backend.  
* Receiving transcription events and routing them to post-processing and text injection.

### Audio Adapter

The Audio Adapter captures and prepares microphone input for inference.

It is responsible for:

* Selecting the microphone source.  
* Capturing audio through the desktop audio stack and Audio Server.  
* Applying audio pre-processing: denoising, voice activity detection (VAD).  
* Resampling audio if required.  
* Maintaining a bounded in-memory rolling buffer for context and overlap.  
* Chunking audio into frames for streaming to the Inference Snap.  
* Stopping capture immediately when the session ends.  
* Releasing the microphone after session completion.

### Audio Server

The Audio Server is an external component providing access to audio hardware and desktop audio services.

It handles:

* Audio device enumeration and selection.  
* Audio format negotiation.  
* Stream management.  
* Low-level audio capture.

### State Management

State Management maintains the session state, including:

* Current session state.
* Audio buffer state.  
* Inference session state.  
* Text input target state.  
* Error state.

### Post-Processing

Post-Processing transforms raw transcription into final committed text.

It is responsible for:

* Text normalization (spoken forms to written forms, e.g., "twenty two" → "22").  
* Capitalization (sentence starts, proper nouns where possible).  
* Punctuation insertion.  
* Formatting (newlines, abbreviations).  
* Commands for advanced text manipulation.  
* Application-aware transfer (context-specific transformations).  
* Optional: Advanced transformations using a Local LLM where available.

Post-processing results should be committed as final stable text.

### Text Injection Layer

The Text Injection Layer abstracts the concrete input method backend and inserts committed text.

It is responsible for:

* Managing the dictation activity indicator lifecycle.  
* Committing stable text to the focused application.  
* Handling focus changes safely.  
* Handling unsupported input targets gracefully.  
* Supporting multiple backends: IBus (current), Wayland input-method protocol (future), xdg text input, and others.  
* Blocking injection into secure fields (password fields, authentication prompts).  
* Ensuring text is injected only into the originally selected target.

### Activity Indicator

The Activity Indicator is a non-intrusive visual element that shows when recording and transcription are active and until the process is finalized.

States such as "Recording", "Transcribing", "Finalizing" and "Error" should be visually distinguishable.

It could be represented as a system tray icon, like a volume meter, or a small overlay near the text input target, depending on what environment can reliably support.

### Settings UI

The Settings UI allows users to configure:

* Enable or disable speech-to-text.  
* Dictation hotkey selection.  
* Dictation language selection.  
* Microphone source selection.  
* Model size or quality profile.  
* Post-processing customization.  
* Activity indicator preferences.

The Settings UI communicates with the Speech Orchestrator through gRPC or DBus (TBD).

## Text Input Backend Strategy

### IBus backend for the first iteration

The first iteration uses IBus as the primary text injection backend.

IBus is suitable for the initial implementation because it provides integration with many desktop applications and a practical commit path for text insertion.

The Text Injection Layer abstracts IBus as an implementation detail, allowing future iterations to introduce additional backends.

### Backend abstraction

The Text Injection Layer defines a text output abstraction independent of any specific backend.

The abstraction supports operations such as:

* Start text session (acquire target focus).  
* Update dictation activity state (show/hide indicator).  
* Commit text (insert text into target).  
* Cancel session (abort without text injection).  
* End session (finalize and release target).

### Wayland-native backend

A Wayland-native input path is planned for future iterations so that speech-to-text is not permanently dependent on IBus.

This is important for:

* Better alignment with the Wayland security model and input method protocol.  
* Better compositor integration and focus handling.  
* More predictable text injection and activity indicator rendering.  
* Reduced dependency on IBus, which is not originally designed as a complete dictation service.  
* Portability to other desktop environments.

The Wayland input-method protocol will eventually replace IBus as the preferred text injection backend.

## Streaming Model

The system is designed around streaming input and streaming output through the Speech Orchestrator.

### Audio streaming

The Audio Adapter captures audio frames and streams them incrementally to the Inference Snap.

The system does not wait until the user releases the hotkey before starting speech recognition. Instead, recognition begins immediately as audio frames arrive at the Inference Snap.

### Transcription streaming

The Inference Snap produces two kinds of transcription output:

* Partial transcription: unstable, changing hypotheses.  
* Final transcription: stable, committed segments.

Partial transcription is treated as internal state and routed through State Management in the MVP. Final transcription is routed through Post-Processing and then to the Text Injection Layer for commit.

### Buffering

The Audio Adapter maintains a bounded in-memory rolling buffer to provide sufficient context for accurate recognition.

The implementation defines:

* Maximum rolling buffer duration (controlled by Audio Adapter).  
* Maximum utterance context duration (Inference Snap model parameter).  
* Whether overlapping windows are used (Inference Snap model parameter).  
* Buffer discarding strategy (cleared on session end by Audio Adapter).  
* Whether buffering parameters vary by model (configurable per model profile).

The buffer must not be persisted to disk. All buffers are cleared when the dictation session ends.

## Model and Language Strategy

### Model selection

The system should support model profiles so that performance, size, and accuracy can be balanced.

Possible profiles include:

* Lightweight profile: smaller model, lower resource usage, reduced accuracy.  
* Default profile: balanced size, latency, and accuracy.  
* Quality profile: larger model, better accuracy, higher resource usage.

The default model should be selected automatically based on:

* Dictation language.  
* Available hardware acceleration.  
* System memory.  
* CPU capability.  
* Installed model packs.  
* Product defaults.

### Language selection

The default dictation language should match the display language when a supported model is available.

The user must be able to change the dictation language independently of the display language.

The selected dictation language should persist across sessions.

### Multilingual support

The implementation may use either language-specific models or multilingual models.

The specification should document the trade-offs:

* Language-specific models may be smaller and faster.  
* Multilingual models may simplify installation and language switching.  
* Larger multilingual models may have higher memory and latency costs.  
* Accuracy may vary significantly by language.

Automatic language detection is out of scope for the first iteration.

## Performance and Quality Requirements

### Latency

The following metrics should be measured on reference hardware:

* Time from hotkey press to microphone capture.  
* Time from hotkey press to activity indicator visible.  
* Time from hotkey press to first transcription event (partial or final).  
* Time from key release to final committed text.  
* Time to model warm-up when the model is not already loaded.  
* Time to recover from idle state.

Suggested initial targets:

* Microphone capture starts within 100 ms.  
* Activity indicator appears within 100–200 ms on reference hardware.  
* First transcription event arrives within 500–800 ms on reference hardware.  
* Final text is committed within 1–2 seconds after key release on reference hardware.

### Accuracy

Accuracy should be measured using word error rate or another agreed metric.

The test matrix should include:

* English, quiet environment.  
* Default non-English language, quiet environment.  
* English with background noise.  
* Default non-English language with background noise.  
* Accented speech.  
* Short commands.  
* Long-form dictation.  
* Technical vocabulary.  
* Names and uncommon words.  
* Acronyms and abbreviations.

Accuracy targets may vary by language and model profile.

### Resource usage

The implementation should define limits or reference measurements for:

* Memory usage.  
* CPU usage.  
* GPU or NPU usage where applicable.  
* Disk footprint.  
* Startup time.  
* Battery impact.  
* Idle resource usage.

### Offline behavior

The feature must work without network connectivity once the required model and runtime are installed.

If the selected model is not installed, the user should receive a clear message explaining what is required.

The system must not silently fall back to a cloud service.

## Compatibility Matrix

The MVP should be validated against a defined application matrix.

Recommended initial matrix:

* GTK text fields.  
* Flutter text fields.  
* GNOME Text Editor.  
* LibreOffice Writer.  
* Firefox.  
* Chromium.  
* Electron application.  
* Terminal emulator.  
* XWayland application.  
* Application with custom text widget.

Each application should be tested for:

* Activity indicator behavior during recording/transcription.  
* Final commit behavior.  
* Focus handling.  
* Cancellation.  
* Language switching.  
* Hotkey behavior.  
* Secure-field handling where applicable.

## Settings and User Controls

The user should be able to configure:

* Enable or disable speech-to-text.  
* Dictation hotkey.  
* Dictation language.  
* Microphone source.  
* Model or quality profile, if exposed.  
* Whether to show an on-screen dictation indicator.

The settings UI should clearly explain that speech recognition runs locally and that audio is not persisted by default.

## Accessibility and Internationalization

The feature should be designed as both a productivity tool and an accessibility feature.

The specification should consider:

* Keyboard-only configuration.  
* Screen reader compatibility.  
* Visible dictation status.  
* Clear error messages.  
* Translatable UI strings.  
* Right-to-left languages.  
* Languages with complex input behavior.  
* Punctuation and capitalization expectations by language.

### Accessibility considerations

The indicator becomes a key accessibility feature to provide feedback that the system is actively listening and transcribing. It should be designed to be perceivable and understandable for users with various disabilities.

It includes states such as "Recording", "Transcribing", "Finalizing" and "Error" that should be visually distinguishable. The indicator should also be announced by screen readers when it appears, providing clear feedback to users relying on assistive technologies.

## Cancellation Handling

The user must be able to cancel an active dictation session at any time by releasing the hotkey or through an explicit cancellation action.

When a session is cancelled, recording should stop immediately, any uncommitted text should be discarded, the activity indicator should be cleared, and the microphone should be released.

## Failure Handling

The system must fail safely. Different components handle different failure scenarios:

* **Speech Orchestrator**: If inference fails, stop the session and preserve already committed text. If the model is unavailable, prevent recording from starting.  
* **Audio Adapter**: If the microphone becomes unavailable, stop listening immediately and notify the user through the Speech Orchestrator.  
* **Text Injection Layer**: If focus is lost during a session, avoid committing text into an unintended target. If the activity indicator cannot be shown in the preferred surface, fall back to a secondary desktop-visible indicator while keeping commit-only insertion behavior.  
* **Post-Processing**: If transformation fails, pass through the raw transcription or discard the segment, depending on severity.  
* **Inference Snap**: If a model fails to load or crashes, signal availability through the Configuration API so clients can show appropriate errors.

All failures should provide clear user feedback through error states documented in the User Experience section.

## Observability and Diagnostics

The system should provide enough diagnostics to debug issues without compromising privacy.

Allowed by default:

* Session start and stop events.  
* Error codes.  
* Backend availability.  
* Model identifier.  
* Language identifier.  
* Latency metrics.  
* Resource usage metrics.

Not allowed by default:

* Raw audio.  
* Full dictated text.  
* Sensitive text field contents.

Any diagnostic mode that captures audio or transcription content must require explicit user consent.

## Testing Strategy

The testing strategy should include:

* Unit tests for session lifecycle.  
* Unit tests for buffering and finalization.  
* Unit tests for text output state transitions.  
* Integration tests with the inference backend.  
* Integration tests with the IBus adapter.  
* Manual and automated application compatibility tests.  
* Offline-mode tests.  
* Secure-field tests.  
* Latency benchmarks.  
* Accuracy benchmarks.  
* Resource usage benchmarks.  
* Regression tests for focus changes and cancellation.

Testing must explicitly verify commit-only insertion behavior in the MVP and correct activity-indicator lifecycle.

## Acceptance Criteria

The feature is acceptable for the first iteration when:

* Dictation can be started and stopped with a hotkey.  
* Audio is captured only during the active session.  
* Speech is transcribed locally without network access.  
* A visible recording/transcription activity indicator is shown during active dictation.  
* Only committed stable text is inserted into target applications in the initial version.  
* Final text is inserted into common desktop applications.  
* IBus is used only through an abstract text output backend.  
* The selected dictation language can be changed by the user.  
* Audio is not persisted by default.  
* Dictation is blocked in secure fields where detectable.  
* Performance and accuracy meet the agreed reference targets.  
* Failures are visible, understandable, and safe.

## Risks and Mitigations

| Risk | Mitigation |
| :---- | :---- |
| **Latency and accuracy trade-off** Small models may not be accurate enough, while larger models may be too slow. | Support model profiles and define hardware-specific targets. |
| **IBus limitations** IBus may not provide reliable behavior across all Wayland applications. | Use IBus only as an initial backend and keep the text output layer abstract. |
| **Reduced immediacy in commit-only UI** Users may perceive lower responsiveness when text is committed in chunks. | Provide a clear low-latency activity indicator and optimize time to first transcription and final commit. |
| **Wrong-target insertion** Dictated text may be committed into the wrong application after focus changes. | Capture the target at session start and avoid dynamic retargeting. |
| **Privacy concerns** Users may fear that the desktop is always listening. | Use push-to-talk only, show clear active-state indication, and never persist audio by default. |
| **Hardware variance** Performance may vary significantly across machines. | Define reference hardware tiers and model profiles. |

## Future Work

Future iterations may include:

* Wayland-native text input backend.  
* Support for additional desktop environments.  
* Text-only or server-side usage.  
* Automatic language detection.  
* Custom vocabulary.  
* User correction interface.  
* Voice commands.  
* Punctuation customization.  
* Continuous dictation mode.  
* Dictation history, only if explicitly enabled by the user.  
* Hardware acceleration through GPU or NPU where available.  
* Integration with broader context-aware desktop features.
