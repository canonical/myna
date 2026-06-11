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
* Stream audio input and transcription output in real time.  
* Distinguish between provisional and final transcription where supported.  
* Use only bounded in-memory buffering and avoid persisting audio by default.  
* Support configurable dictation languages, defaulting to the user's display language when available.  
* Operate safely within the Wayland security model and avoid inserting text into unintended targets.  
* Provide an architecture that can support multiple text input backends in future implementations.

### **Non-goals**

The following features are out of scope:

* Wake-word activation and background listening.  
* Continuous dictation sessions.  
* Cloud-based speech recognition services.  
* Voice assistants, voice commands, or desktop control.  
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
5. The system displays provisional transcription as the user speaks.  
6. Stable text is committed into the focused application.  
7. The user releases the hotkey.  
8. The system finalizes the current utterance.  
9. The microphone capture stops.

### Target selection

The text input target is selected when the user presses the hotkey.

If focus changes during an active dictation session, the system must not silently commit dictated text into an unrelated application. The implementation should either continue targeting the original text surface or cancel/finalize the session safely, depending on what the text input backend can guarantee.

### Partial and final text

The system must distinguish between provisional and committed text.

Partial transcription may be revised as more speech context becomes available. Therefore, partial transcription must not be treated as permanently inserted text unless the input backend supports replacing it safely.

Where supported, partial transcription should be displayed using preedit text. Stable transcription should be inserted using commit text.

This distinction is required to avoid corrupting the user’s document with unstable model output.

#### **Provisional Text**

Provisional text is the text produced while the user is still speaking and the model has incomplete acoustic and linguistic context.

It can be visible to the user, but it should visually be distinct from the committed text. For example:

I would like to schedule a meeting with the team *tomorrow after*

The light grey, italic part, would be provisional and it may still change as more audio arrives. It is similar to Japanese or Chinese input methods, or accented characters on non-accented characters keyboards. It allows the system to replace the text without creating a visible “typing and deleting” noise.

Once the model is confident enough, or once the hotkey is released, the provisional segment is converted into final text.

Of course, it could be a commit-only rendering. In this case, the provisional text is not shown at all.  It is simpler, but the experience feels less responsive because the text appears in chunks.

#### **Final Text**

The final text is committed into the target application and should no longer be modified, except perhaps by an explicit correction command or later post-processing right before commit.

#### **Example flow**

1. User says:  
    Lets meet tomorrow afternoon

2. Provisional output appears progressively:  
    Lets meet tomorrow *after*

3. More context arrives, the text is updated:  
    Lets meet tomorrow *afternoon*

4. Finally the phrase is committed as final text:  
    Lets meet tomorrow afternoon

#### **Post-processing**

Post-processing could happen at several stages to:

* Normalize the text e.g. convert spoken forms to written forms where appropriate (twenty two \-\> 22\)  
* Punctuation  
* Capitalization e.g. sentence starts, proper nouns where possible  
* Formatting e.g. newlines if possible, abbreviations  
* Safety filtering e.g. detect sensitive contexts

### End of session

When the user releases the hotkey, the system must:

* Stop accepting new audio input.  
* Finalize any buffered speech.  
* Commit the final stable text.  
* Clear any remaining provisional text.  
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

Special care is required for:

* Password fields.  
* Lock screen.  
* Polkit prompts.  
* Terminal emulators.  
* Remote desktop sessions.  
* Virtual machine windows.  
* Browser address bars.  
* Applications using custom text widgets.

## Architecture

The high level architecture is composed of 4 main layers.

![][image1]

### Dictation session controller

The session controller owns the lifecycle of a dictation session.

It is responsible for:

* Global hotkey handling.  
* Starting and stopping dictation sessions.  
* Tracking the current text input target.  
* Managing session state.  
* Coordinating audio capture, inference, and text output.  
* Handling cancellation and errors.  
* Enforcing privacy and security rules.

### Audio pipeline

The audio pipeline captures microphone input and prepares it for inference.

It is responsible for:

* Selecting the microphone source.  
* Capturing audio through the desktop audio stack.  
* Resampling audio if required.  
* Applying voice activity detection where appropriate.  
* Maintaining a bounded in-memory buffer.  
* Chunking audio for streaming inference.  
* Stopping capture immediately when the session ends.

### Inference pipeline

The inference pipeline sends streamed audio to the Canonical Inference Snap and receives transcription events.

It is responsible for:

* Selecting the appropriate model.  
* Starting an inference session.  
* Streaming audio frames.  
* Receiving partial transcription.  
* Receiving final transcription.  
* Applying punctuation and casing where supported.  
* Reporting confidence and stability information where available.  
* Handling model or runtime failures.

### Text output pipeline

The text output pipeline converts transcription events into text insertion operations.

It is responsible for:

* Displaying provisional text.  
* Committing stable text.  
* Replacing or clearing provisional text.  
* Handling focus changes.  
* Handling unsupported input targets.  
* Abstracting the concrete input backend.

The first implementation uses an IBus adapter. The architecture must allow future adapters, including a Wayland-native input method backend.

## Text Input Backend Strategy

### IBus backend for the first iteration

The first iteration may use IBus to provide preedit and commit text behavior.

IBus is suitable for the initial implementation because it already provides concepts that match streaming dictation:

* Preedit text for provisional transcription.  
* Commit text for finalized transcription.  
* Integration with many desktop applications.

However, IBus must be treated as an implementation backend, not as the core architecture.

### Backend abstraction

The dictation system must define a text output abstraction independent of IBus.

The abstraction should support operations such as:

* Start text session.  
* Update provisional text.  
* Commit text.  
* Clear provisional text.  
* Cancel session.  
* End session.

### Wayland-native backend

We should add a Wayland-native input path so that speech-to-text does not permanently depend on IBus.

This is important for:

* Better alignment with the Wayland security model.  
* Better compositor integration.  
* More predictable focus handling.  
* Reduced dependency on an input method framework not originally designed as a complete dictation service.  
* Portability to other desktop environments.

## Streaming Model

The system must be designed around streaming input and streaming output.

### Audio streaming

Audio must be sent to the inference backend incrementally. The system must not wait until the user releases the hotkey before starting speech recognition.

### Transcription streaming

The inference backend should produce two kinds of output:

* Partial transcription.  
* Final transcription.

Partial transcription may change. Final transcription is stable and can be committed to the application.

### Buffering

A bounded in-memory buffer may be used to provide sufficient context for accurate recognition.

The implementation must define:

* Maximum rolling buffer duration.  
* Maximum utterance context duration.  
* Whether overlapping windows are used.  
* When buffered audio is discarded.  
* Whether buffering parameters vary by model.

The buffer must not be persisted to disk.

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
* Time from hotkey press to first partial transcription.  
* Time from speech input to visible provisional text.  
* Time from key release to final committed text.  
* Time to model warm-up when the model is not already loaded.  
* Time to recover from idle state.

Suggested initial targets:

* Microphone capture starts within 100 ms.  
* First partial transcription appears within 500–800 ms on reference hardware.  
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

* Partial text behavior.  
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

## Failure Handling

The system must fail safely.

Examples:

* If inference fails, stop the session and preserve already committed text.  
* If the microphone becomes unavailable, stop listening and notify the user.  
* If focus is lost, avoid committing text into an unintended target.  
* If the input backend cannot display preedit text, either commit only stable text or show a temporary overlay.  
* If the model is unavailable, do not start recording audio.

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

Testing must explicitly verify the distinction between provisional and committed text.

## Acceptance Criteria

The feature is acceptable for the first iteration when:

* Dictation can be started and stopped with a hotkey.  
* Audio is captured only during the active session.  
* Speech is transcribed locally without network access.  
* Partial and final transcription are handled separately.  
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
| **Provisional text corruption** Partial transcription may be inserted permanently before it is stable. | Use preedit text where possible and commit only final text. |
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
