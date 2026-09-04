# Feature Specification: Desktop Session Controller + Text Injection

**Feature Branch**: `003-desktop-injection`

**Created**: 2026-07-19

**Status**: Draft

## Clarifications

### Session 2026-07-19 (informed defaults; see Assumptions)

- Q: Where does the injection + session controller live — the Python `server/` side or the Rust client? → A: The Rust client. The desktop last-mile is a shipped system component (constitution: shipped components are Rust); the Python `server/src/myna/desktop/` stubs are legacy vocabulary and are retired by this feature. The Rust orchestrator already carries the `Trigger` (T21) and `TextSink` (T22) seams these stubs described.
- Q: Which text-injection backend for the first iteration? → A: IBus (UD129 decision). The Wayland-native input-method path (`zwp_input_method_v2`) is not implemented by mutter/GNOME for third-party input methods, so it is future/portability work, not iteration 1 (see Assumptions).
- Q: Does this feature build the full GNOME Settings panel? → A: No. Activation binds through the `org.freedesktop.portal.GlobalShortcuts` portal, whose *rebinding UI is provided by the desktop itself*. This feature ships the in-session activity indicator and the shortcut registration; a dedicated Settings panel (language/mic/model pickers) is out of scope (future, UD129 Settings UI).
- Q: What happens to focus if it changes mid-session (IBus follows focus)? → A: The target is captured at session start and is never retargeted. Because IBus cannot guarantee commit into the original surface after a focus change, a focus change away from the captured target ends the session safely (finalize already-committed text, discard the rest) rather than committing into a different application.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Speak into the focused application (Priority: P1) 🎯 MVP

A person places the text cursor in an application (a text editor, chat box, browser field), starts a dictation session, speaks, and ends the session. The words they spoke appear as committed text in that application — the last mile that turns a transcript into keystrokes in the focused app.

**Why this priority**: This is the reason the feature exists. Everything the project has built so far (capture → FSM → transcript) stops one step short of the user's goal: text in their app. Injection is that step. With only this story, myna is a working dictation tool (driven by any trigger, including the existing terminal one), even before the hands-free hotkey and the on-screen UI land.

**Independent Test**: With a focused text field and a running inference backend, run a dictation session driven by the existing (non-portal) trigger, speak a known utterance, end the session, and assert the committed transcript appears in the focused field and nothing is typed elsewhere.

**Acceptance Scenarios**:

1. **Given** a focused editable text field and a resident model, **When** the user starts a session, speaks a known utterance, and ends the session, **Then** the committed transcript is inserted into that field and matches the expected text.
2. **Given** a session produces several committed segments, **When** each `transcription.final` arrives, **Then** each committed segment is inserted in order and once, and never modified afterwards.
3. **Given** a session where the model is still loading at start, **When** the user speaks during the cold-load window, **Then** speech is buffered (not lost) and the eventual transcript is injected once the model is ready — capture-at-press behavior is preserved end-to-end.
4. **Given** an unstable/partial hypothesis (`snippet`) is received, **When** it arrives, **Then** it is NOT inserted into the target application (commit-only: only stable text is ever injected).
5. **Given** a session that ends with no recognized speech, **When** it finalizes, **Then** nothing is inserted and the target field is left unchanged.

---

### User Story 2 - Activate hands-free with a global shortcut (Priority: P2)

A person dictates without touching a terminal: they hold a configured keyboard shortcut, speak, and release it. Holding the shortcut records; releasing it ends the utterance and commits the text. The shortcut is registered as a desktop GlobalShortcut entry and can be rebound through the desktop's own shortcut settings.

**Why this priority**: Push-to-talk via a global hotkey is what makes dictation usable in daily work — the UD129 primary activation model. It builds directly on US1's injection path by replacing the stand-in trigger with the real system hotkey. Without US1, a working hotkey has nowhere to send text; hence P2.

**Independent Test**: Register the shortcut, bind it (accepting the preferred trigger), then press-and-hold it over a focused field, speak, and release; assert a session starts on press, ends on release, and the transcript is injected — with no terminal involved.

**Acceptance Scenarios**:

1. **Given** the shortcut is registered and bound, **When** the user presses and holds it, **Then** a dictation session starts (capture begins) and continues while held.
2. **Given** a session is active from a held shortcut, **When** the user releases the shortcut, **Then** the session finalizes (graceful stop) and the committed transcript is injected.
3. **Given** the compositor emits key-autorepeat while the shortcut is held, **When** repeated activation signals arrive, **Then** only the first starts a session and the rest are ignored until release (no restart/churn).
4. **Given** no session is active, **When** the shortcut has not been pressed, **Then** the microphone is not captured (push-to-talk only; no background listening).
5. **Given** the user wants a different key, **When** they rebind the shortcut through the desktop's shortcut UI, **Then** the new binding activates dictation and the old one no longer does.

---

### User Story 3 - See that dictation is active (Priority: P2)

A person gets clear, non-intrusive visual feedback that dictation is recording, transcribing, finalizing, or has errored — so they know the system is listening and when it is safe to stop speaking. Because partial text is never shown in their application (commit-only), this indicator is how the gap before commit is communicated.

**Why this priority**: UD129 makes the activity indicator an acceptance requirement and an accessibility feature — in a commit-only model the user has no other signal that the system heard them. It rides on US1's session lifecycle. P2 alongside the hotkey because the two together are the usable experience.

**Independent Test**: Drive a session through its lifecycle and assert the indicator shows a distinct state for recording, transcribing, finalizing, and error, appears within the latency target after activation, and clears when the session ends or is cancelled.

**Acceptance Scenarios**:

1. **Given** a session starts, **When** capture begins, **Then** a visible "recording/listening" indicator appears within the activation-latency target.
2. **Given** transcription is in progress, **When** the model is decoding, **Then** the indicator reflects a distinct "transcribing" state.
3. **Given** the user ends the session, **When** finalization completes, **Then** the indicator clears.
4. **Given** an error occurs (no microphone, model unavailable, backend down, secure field), **When** the session cannot start or continue, **Then** the indicator shows a distinct error state and the user gets a clear message.
5. **Given** a screen-reader user, **When** the indicator changes state, **Then** the state is exposed to assistive technology (perceivable, not purely visual).

---

### User Story 4 - Safe targeting and protected fields (Priority: P3)

A person's dictated text only ever lands where they intended, and never in a password box or on the lock screen. The target is fixed at the moment they start the session; if they switch windows while dictating, text does not leak into the new window; if they aim at a password field, dictation refuses with a clear message.

**Why this priority**: These are the UD129 privacy/safety acceptance criteria. They protect against the worst failure modes (wrong-target insertion, secrets typed into the wrong place). Lower priority only because a first internal demo can precede full hardening — but required before the feature is shippable.

**Independent Test**: Start a session targeting field A; switch focus to field B mid-session; assert no text is committed into B (session ends safely instead). Separately, focus a password field and attempt to start; assert dictation refuses with feedback.

**Acceptance Scenarios**:

1. **Given** a session targeting application A, **When** focus moves to application B during the session, **Then** no dictated text is inserted into B; the session finalizes already-committed text and stops (no silent retarget).
2. **Given** the targeted surface disappears (window closed/minimized) mid-session, **When** the loss is detected, **Then** the session is cancelled safely, uncommitted text is discarded, and the user is notified.
3. **Given** a focused field advertises a secure/password content type, **When** the user tries to start a session, **Then** dictation is refused and the user is told why.
4. **Given** a context where secure-input state is not detectable, **When** the user dictates, **Then** the residual risk is documented and behavior is best-effort (per UD129).
5. **Given** any session, **When** text is injected, **Then** only literal committed text is sent — no potentially unsafe key combinations (e.g. Tab, Alt+Tab, Super, function keys) are synthesized.

---

### Edge Cases

- **Focus lost to another surface mid-session**: default is to finalize-and-stop rather than retarget (US4-1); the captured target is authoritative for the session's lifetime.
- **Target application closes mid-session**: cancel safely, discard uncommitted text, notify (US4-2).
- **Model still loading when the user starts speaking**: buffered at press, injected once ready (US1-3); the activity indicator distinguishes "preparing" from "recording".
- **Hotkey autorepeat while held**: dedupe to a single session start (US2-3).
- **Session ends before the backend emits a terminal event**: finalize on what was committed; do not synthesize text; surface the truncation as needed.
- **Empty / no-speech session**: nothing injected, target unchanged (US1-5).
- **Secure field focused**: refuse with feedback (US4-3); where undetectable, best-effort (US4-4).
- **Injection backend unavailable** (IBus not running / not reachable): session cannot deliver text → clear error, no silent loss.
- **Non-editable or no focused text surface at start**: dictation cannot target anything → clear error rather than a silent no-op.
- **Very long dictation**: committed segments stream in as they finalize; no unbounded buffering of uncommitted text.

## Requirements *(mandatory)*

### Functional Requirements

#### Session control (T21)

- **FR-001**: The system MUST provide a desktop session controller that owns the dictation lifecycle: on activation it captures the target and starts audio + an inference session; on deactivation it finalizes and tears down; audio buffers are discarded at session end.
- **FR-002**: The controller MUST drive the existing session/residency state model (capture-at-press, push gated on model readiness) so speech during a cold-load window is buffered and not lost.
- **FR-003**: The controller MUST route stable committed text (`transcription.final` / terminal `transcription.done`) to text injection, and MUST NOT route unstable hypotheses (`snippet`) to injection.
- **FR-004**: The controller MUST enforce push-to-talk: the microphone is captured only during an active, user-initiated session; there MUST be no background/continuous listening.
- **FR-005**: The controller's state transitions MUST be validated against an explicit legal-transition model, and error/cancel states MUST surface user feedback.

#### Activation (GlobalShortcut) (T21)

- **FR-006**: The system MUST be activatable through a desktop GlobalShortcut entry, registered so the user can bind and rebind it through the desktop's own shortcut UI.
- **FR-007**: Activation MUST support hold-to-talk semantics: the activation edge starts a session and the deactivation edge ends it (graceful stop).
- **FR-008**: The system MUST dedupe autorepeat activation signals so a single held shortcut starts exactly one session until it is released.
- **FR-009**: The activation shortcut MUST default to a binding that does not conflict with reserved desktop shortcuts and MUST NOT be a modifier-only trigger; the exact default is configurable and confirmable by the user at bind time.
- **FR-010**: The activation boundary MUST sit behind the same trigger abstraction the orchestrator already exposes, so the real portal hotkey and a test/stand-in trigger are interchangeable without changing the controller.

#### Text injection (T22)

- **FR-011**: The system MUST insert committed text into the application that was focused when the session started, using IBus as the first-iteration backend behind a backend-agnostic injection abstraction.
- **FR-012**: Injection MUST be commit-only for the MVP: committed text is inserted as stable text and never modified afterward; provisional/preedit text is not shown in the target application.
- **FR-013**: The injection abstraction MUST support the lifecycle operations: acquire target at session start, show/hide activity state, commit stable text, cancel (abort without further injection), and end (finalize and release). Cancel and end MUST be idempotent.
- **FR-014**: The injection target MUST be captured at session start and MUST NOT be retargeted mid-session; on a focus change away from the captured target the session MUST end safely rather than commit into a different application.
- **FR-015**: Injection MUST insert only literal text and MUST NOT synthesize potentially unsafe key combinations (e.g. Tab, Alt+Tab, Super, function keys).
- **FR-016**: The injection backend MUST be replaceable without changing callers, so a future Wayland-native input-method backend can be added behind the same abstraction.

#### Activity indicator / UI (T22)

- **FR-017**: The system MUST display a visible, non-intrusive activity indicator while a session is active, with visually distinct states for recording, transcribing, finalizing, and error.
- **FR-018**: The indicator MUST appear within the activation-latency target after the session starts and MUST clear when the session ends or is cancelled.
- **FR-019**: The indicator's state changes MUST be exposed to assistive technologies (screen-reader perceivable), per UD129 accessibility requirements.
- **FR-020**: When the preferred indicator surface is unavailable, the system SHOULD fall back to a secondary desktop-visible indicator while preserving commit-only behavior.

#### Safety, privacy, errors

- **FR-021**: The system MUST block dictation in secure input fields (password fields, lock screen, authentication prompts) where the secure/password content type is detectable, refusing to start with clear feedback; where undetectable, protection is best-effort and the residual risk MUST be documented.
- **FR-022**: If the targeted surface disappears mid-session, the system MUST cancel safely, discard uncommitted text, and notify the user.
- **FR-023**: The system MUST give clear, actionable feedback for start/continue failures: no microphone, microphone permission denied, model not installed/unavailable, unsupported language, inference backend unavailable, no compatible focused target, or blocked secure field.
- **FR-024**: The system MUST NOT persist audio to disk and MUST discard the in-memory buffer at session end; diagnostics MUST NOT include raw audio or full transcription content by default.

#### Legacy migration / cleanup

- **FR-025**: The legacy Python desktop stubs (`server/src/myna/desktop/`: the `DictationState` vocabulary and `TextInjector` protocol) MUST be retired; their contract now lives in the Rust client. Removing them MUST NOT break the Python server, testbed, or test suite (they are interface-only stubs with no runtime dependents).

### Key Entities *(include if feature involves data)*

- **Dictation session**: one push-to-talk utterance lifecycle — its state (idle/starting/recording/transcribing/finalizing/completed/cancelled/error), the captured target, and the bounded in-memory audio buffer (discarded at end).
- **Activation trigger**: a source of press/release edges bounding a session — the real desktop GlobalShortcut binding and any stand-in trigger implement the same abstraction.
- **Injection target**: the editable text surface focused at session start — the sole destination for committed text for the session's lifetime; carries detectable content-type/secure-field state.
- **Injection backend**: the concrete text-insertion mechanism (IBus first) behind a backend-agnostic abstraction with the acquire/indicate/commit/cancel/end lifecycle.
- **Activity indicator**: the visible, assistive-tech-exposed session-state signal (recording/transcribing/finalizing/error).
- **Committed segment**: stable transcribed text routed to injection; never retracted or modified after commit.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A known spoken utterance dictated into a focused editable field appears there as committed text matching the expected transcript, with nothing inserted into any other surface.
- **SC-002**: Across a defined application matrix (e.g. GNOME Text Editor, a GTK field, a browser field, a chat/Electron field, a terminal), committed dictation lands correctly in each supported target, with unsupported targets failing visibly rather than silently.
- **SC-003**: Holding the bound global shortcut starts a session and releasing it ends and commits it, hands-free, with no terminal involved, in 100% of trials; a held shortcut starts exactly one session regardless of autorepeat.
- **SC-004**: The microphone is captured only while a session is active; in an idle state, no audio is captured (verifiable: no capture stream exists between sessions).
- **SC-005**: The activity indicator becomes visible within the UD129 activation-latency target (≈100–200 ms on reference hardware) after activation and clears within the session-teardown target after the session ends.
- **SC-006**: Committed-only behavior holds: no partial/unstable hypothesis is ever inserted into a target application across the test suite.
- **SC-007**: A focus change away from the captured target during a session results in zero characters inserted into the new surface, in 100% of trials.
- **SC-008**: Attempting to dictate into a detectable secure/password field is refused with user feedback in 100% of trials where the secure content type is exposed.
- **SC-009**: No audio file is written to disk during any dictation session, and the in-memory buffer is released at session end.
- **SC-010**: The Python server, testbed, and full test suite remain green after the legacy `server/src/myna/desktop/` stubs are removed.

## Assumptions

- **Shipped Rust component**: the session controller, injector, and UI are shipped system components and are implemented in the Rust client (constitution Principle I/Technology constraints). The Python `server/src/myna/desktop/` stubs are legacy interface-only vocabulary and are removed by this feature; the Rust orchestrator's existing `Trigger`/`TextSink` seams are the contracts they described.
- **Reuse of the existing client path**: the capture → session/residency FSM → transcript path (native PipeWire capture, the orchestrator, both wire dialects) is done and reused unchanged; this feature adds the *trigger* (real hotkey), the *sink* (IBus injector), the *UI* (indicator), and the session controller that composes them for the desktop.
- **IBus first, Wayland-native later**: IBus is the iteration-1 injection backend (UD129). Upstream review confirms the Wayland-native input-method side (`zwp_input_method_v2` / `xx-input-method-v2`) is not implemented by mutter/GNOME for third-party input methods, so it is future/portability work (e.g. wlroots compositors); `text-input-v3` is the application↔compositor protocol apps use, not something this feature implements. The injection abstraction keeps that future backend addable without touching callers.
- **Activation via `org.freedesktop.portal.GlobalShortcuts`**: hold-to-talk is delivered by the portal's `Activated`/`Deactivated` signals (source-verified on GNOME 50; upstream, not an Ubuntu patch); the portal grabs the accelerator so no shell extension is needed, and the desktop provides the rebinding UI. Autorepeat may re-emit activation and is deduped client-side.
- **Primary environment**: Ubuntu Desktop on Wayland with GNOME as the validated desktop; portability to other environments is a design constraint, not a delivery target for iteration 1.
- **UI scope**: this feature ships the in-session activity indicator and the shortcut registration/binding path. A dedicated GNOME Settings panel (language/microphone/model pickers, enable toggle) is out of scope here (future UD129 Settings UI); where a shortcut must be rebound, the desktop's own portal-provided UI is used.
- **Commit-only MVP**: provisional/preedit rendering in the target application is out of scope (UD129 defers it to a future iteration where replacement safety is guaranteed by the backend). Post-processing (normalization, punctuation, filler removal, custom-word biasing) beyond what the inference backend already emits is out of scope for the MVP unless trivially available.
- **Focus-change safety over continuity**: because IBus follows input focus and cannot guarantee commit into the original surface after a focus change, the safe default (end-the-session) is chosen over UD129's optional "keep targeting original surface" where the backend cannot honor it.
- **Inference backend**: the existing `myna-server` (or a shipped inference snap) stands in as the transcription backend over the established transport; model residency/idle-unload policy is owned elsewhere (plan T29) and only consumed here.
- **Language/model**: single dictation language per session, configured at/near session start; automatic language detection and translation remain out of scope (UD129 non-goals).

## Out of Scope

- Wake-word / always-on listening, continuous dictation, voice commands/desktop control, translation, diarization, dictation history, and audio retention (UD129 non-goals).
- A dedicated GNOME Settings panel for STT (language/mic/model/enable); full post-processing pipeline; provisional/preedit in-app rendering.
- A Wayland-native input-method backend (kept addable, not delivered here).
- Guaranteed compatibility with every toolkit/custom text widget; XWayland and custom-widget targets are best-effort.
