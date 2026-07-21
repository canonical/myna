# Feature Specification: GNOME Shell Extension for Myna Dictation UI

**Feature Branch**: `004-gnome-shell-indicator`

**Created**: 2026-07-21

**Status**: Draft

**Input**: User description: "Design and implement a GNOME Shell extension for Ubuntu 26.04+ (GNOME 48/49) that provides a stunning, animated visual interface for the Myna dictation system. The extension solves the Wayland focus-stealing problem that prevents client-side overlays on GNOME by running inside the compositor. Core experience: a 'goop' UI element hanging from the top bar, visible during dictation, alive with a Gemini-Live-style activity pulse, audio-level feedback, and color-coded model states. The extension is pure UI — it visualizes state exposed by the existing myna-desktop process over D-Bus and optionally provides start/stop triggers, but does not perform transcription or injection itself."

## Clarifications

### Session 2026-07-21 (informed defaults; see Assumptions)

- Q: Does this feature include text injection, or is it UI-only? → A: UI-only. IBus injection (feature 003) stays and is toolkit-agnostic; the shell's direct Clutter text access covers only Clutter/GTK widgets and would regress coverage for Qt/Electron/Firefox. The extension visualizes state and optionally triggers start/stop; it never commits text.
- Q: One extension or two (UI vs injection)? → A: One extension, UI only (landscape "Option A"). Injection remains in `myna-desktop`.
- Q: How does the extension learn dictation state and audio levels? → A: Over a session-bus D-Bus interface exposed by `myna-desktop`. The extension is a consumer; the interface contract (state property + change signal, audio-level properties, optional start/stop/toggle methods) is defined by this feature but implemented on the `myna-desktop` side.
- Q: Does the extension replace the feature-003 activity indicator? → A: On GNOME it becomes the preferred indicator surface (FR-020 fallback in feature 003); `myna-desktop`'s own indicator remains the fallback when the extension is absent. Commit-only privacy behavior is unchanged.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See dictation state without losing focus (Priority: P1) 🎯 MVP

A person is typing in an application, starts a dictation session (via the existing hotkey), and sees an animated indicator appear — hanging from the top bar — that clearly shows the system is listening. It never steals focus from the app they are typing into, so dictation is not broken by the indicator itself. When the session ends, the indicator clears.

**Why this priority**: This is the reason the extension exists. On GNOME/Wayland a normal client cannot show an always-on-top, non-focus-stealing overlay; the feature-003 notification indicator is constrained and can steal focus. Running inside the compositor eliminates the focus problem and is the minimum that delivers value. With only this story, users get a reliable, focus-safe "dictation is active" signal that the client cannot otherwise provide.

**Independent Test**: With the extension installed and `myna-desktop` running, start a dictation session while a text field is focused; assert the indicator becomes visible, that keyboard focus remains in the original text field (typing still lands there), and that the indicator clears when the session ends.

**Acceptance Scenarios**:

1. **Given** the extension is installed and `myna-desktop` is running, **When** a dictation session starts, **Then** an animated indicator becomes visible hanging from the top bar within the activation-latency target.
2. **Given** the indicator is visible during an active session, **When** the user continues to interact with the focused application, **Then** keyboard focus is never taken by the indicator and typed input still lands in the focused application.
3. **Given** a dictation session is active, **When** the session ends or is cancelled, **Then** the indicator clears (no persistent presence while idle).
4. **Given** no dictation session is active (idle), **When** the user is working normally, **Then** no overlay is shown (push-to-talk: no background presence).
5. **Given** `myna-desktop` is not running or its interface is unavailable, **When** the extension loads, **Then** the extension stays dormant (no overlay, no errors surfaced to the user) and activates when the interface becomes available.

---

### User Story 2 - Read the current dictation state at a glance (Priority: P1)

A person can tell, from the indicator alone, whether the system is loading a model, listening, transcribing, finalizing, or has errored — because partial transcript text is never shown in their application (commit-only), the indicator is how the pre-commit gap is communicated.

**Why this priority**: State legibility is the core UX payload. A focus-safe overlay that does not distinguish states would not tell the user when it is safe to speak or stop. This is co-P1 with US1: together they are the MVP (a visible, state-legible, focus-safe indicator).

**Independent Test**: Drive `myna-desktop` (or a stand-in publisher of the same interface) through each state and assert the indicator shows a visually distinct treatment for each of idle, loading/preparing, recording, transcribing, finalizing, and error, and transitions between them promptly.

**Acceptance Scenarios**:

1. **Given** a session where the model is still loading, **When** the loading/preparing state is active, **Then** the indicator shows a distinct "model loading" treatment (e.g. a warm/red glow) different from the listening treatment.
2. **Given** the model is resident and the system is listening, **When** the recording state is active, **Then** the indicator shows an active "listening" animation distinct from all other states.
3. **Given** the system is transcribing, **When** the transcribing state is active, **Then** the indicator shows a processing treatment distinct from listening.
4. **Given** the session is finalizing, **When** the finalizing state is active, **Then** the indicator shows a brief confirmation/finalizing treatment.
5. **Given** an error occurs, **When** the error state is active, **Then** the indicator shows a clearly distinct error treatment (e.g. a red flash) and surfaces the error briefly.
6. **Given** the interface publishes a state the extension does not recognize, **When** it is received, **Then** the extension degrades gracefully (falls back to a neutral active/idle treatment) rather than breaking.

---

### User Story 3 - See that my voice is being captured (Priority: P2)

A person sees real-time feedback that their voice is actually being picked up — a VU-style level or a glow whose intensity tracks captured audio level — so they know the microphone is working and they are speaking at a usable volume.

**Why this priority**: Level feedback answers the most common failure ("is it hearing me?") and makes the UI feel alive. It builds on US1/US2 (the indicator must already exist and show state) and requires an audio-level stream that may not be present in the very first slice, hence P2.

**Independent Test**: With a session active, feed known audio levels through the interface and assert the indicator's level representation tracks them (rises with louder input, falls with silence) at a smooth, responsive update rate, and that it shows no level when idle.

**Acceptance Scenarios**:

1. **Given** an active recording session, **When** the captured audio level rises, **Then** the indicator's level representation increases correspondingly and smoothly.
2. **Given** an active recording session, **When** input goes silent, **Then** the level representation falls toward its floor.
3. **Given** no session is active, **When** idle, **Then** no audio level is displayed.
4. **Given** the interface stops publishing level updates (stale), **When** updates lapse beyond a short window, **Then** the level representation decays to its floor rather than freezing at the last value.

---

### User Story 4 - Start or stop dictation from the panel (Priority: P3)

A person who prefers a pointer to a hotkey can click a subtle panel presence to toggle dictation on and off, without needing to remember or reach the keyboard shortcut.

**Why this priority**: A click-to-toggle affordance is a convenience layered on top of the visualization; the hotkey (feature 003) already provides activation. It depends on the D-Bus command surface and the panel presence, so it is the lowest priority slice.

**Independent Test**: With the extension installed and `myna-desktop` running, click the panel affordance and assert a session starts (state moves out of idle); click again (or click stop) and assert the session ends — with the same commit-only behavior as the hotkey path.

**Acceptance Scenarios**:

1. **Given** no session is active, **When** the user clicks the panel toggle, **Then** a dictation session starts (equivalent to the hotkey press) and the indicator reflects it.
2. **Given** a session is active, **When** the user clicks the panel toggle (or a stop control), **Then** the session ends gracefully and committed text behavior matches the hotkey path.
3. **Given** the command surface is unavailable, **When** the user clicks the toggle, **Then** the extension gives non-intrusive feedback rather than failing silently, and does not leave a stuck visual state.

---

### Edge Cases

- **`myna-desktop` absent or interface unavailable at load**: extension stays dormant, no overlay, no user-facing errors; activates when the interface appears (US1-5).
- **`myna-desktop` disappears mid-session** (crash / disconnect): indicator clears to idle rather than freezing in an active state.
- **Unknown/extra state value** published by the interface: degrade to a neutral treatment (US2-6).
- **Stale audio-level stream**: level decays to floor rather than freezing (US3-4).
- **GNOME Shell version mismatch**: extension declares its supported Shell versions and does not attempt to load on unsupported versions (see Assumptions).
- **Shell lock/restart** (`Alt+F2 r` on X11 is unavailable on Wayland; session relogin): extension re-initializes cleanly and reconnects to the interface; no leaked actors or timers.
- **Rapid state churn** (fast start/stop): indicator does not accumulate overlapping animations or leak actors; transitions coalesce.
- **High-contrast / accessibility mode**: indicator remains legible; state changes are perceivable by assistive technology.
- **Multi-monitor / panel on a specific monitor**: indicator positions relative to the top bar consistently and does not appear off-screen.

## Requirements *(mandatory)*

### Functional Requirements

#### Overlay & focus safety

- **FR-001**: The system MUST present a dictation indicator that runs inside the GNOME Shell compositor and MUST NOT take keyboard focus from the user's focused application at any point in the session lifecycle.
- **FR-002**: The indicator MUST be visible during an active dictation session and MUST clear when the session ends, is cancelled, or errors out — there MUST be no persistent overlay while idle (push-to-talk).
- **FR-003**: The indicator MUST appear within the activation-latency target after a session starts and MUST clear within the teardown target after it ends (consistent with feature 003 timing targets).
- **FR-004**: The indicator MUST position relative to the top bar (centered "goop"/hanging element) and MUST render correctly in single- and multi-monitor layouts without appearing off-screen.

#### State visualization

- **FR-005**: The indicator MUST show visually distinct treatments for each dictation state: idle (hidden), loading/preparing (model load in progress), recording/listening, transcribing, finalizing, and error.
- **FR-006**: The model-loading/preparing state MUST be visually distinct from the listening state so a cold-model load is legible as "loading" and not mistaken for "listening".
- **FR-007**: The error state MUST be clearly distinguished and MUST briefly surface that an error occurred, then return to idle when the session clears.
- **FR-008**: The indicator MUST degrade gracefully when it receives an unrecognized state value, falling back to a neutral active/idle treatment rather than breaking.
- **FR-009**: Animations MUST be smooth and MUST NOT block or visibly stutter the compositor; animations MUST stop and their resources be released when the session clears (no accumulation across rapid start/stop cycles).

#### Audio-level feedback

- **FR-010**: The indicator MUST provide a real-time audio-level representation (VU-style level and/or glow intensity) tied to the captured voice level during recording, updated at a smooth, responsive rate.
- **FR-011**: The audio-level representation MUST show no level while idle and MUST decay toward its floor when the level stream goes stale or silent (never freeze at the last value).
- **FR-012**: The audio-level representation MUST convey only level/energy and MUST NOT render or leak any transcript content.

#### Panel presence & triggers

- **FR-013**: The system MAY provide a panel presence (tray/top-bar button); if present, it MUST follow GNOME Human Interface Guidelines for panel buttons and be subtle/non-intrusive.
- **FR-014**: If a panel trigger is provided, it MUST allow the user to start and stop/toggle a dictation session, equivalent in effect to the existing hotkey activation, preserving commit-only behavior.
- **FR-015**: When a trigger command is unavailable, the extension MUST give non-intrusive feedback and MUST NOT leave a stuck visual state.

#### Integration with myna-desktop (D-Bus)

- **FR-016**: The extension MUST obtain dictation state and audio levels from the existing `myna-desktop` process over a session-bus D-Bus interface, and MUST NOT capture audio, perform transcription, or inject text itself.
- **FR-017**: The D-Bus interface MUST expose, at minimum: the current dictation state, a state-change notification, and audio-level values; and MAY expose start/stop/toggle commands and an error message. The interface contract is defined by this feature and implemented on the `myna-desktop` side.
- **FR-018**: The extension MUST tolerate `myna-desktop` being absent at load, appearing later, and disappearing mid-session: it stays dormant when the interface is unavailable, activates when it appears, and clears to idle if it disappears — without surfacing errors to the user for these expected conditions.
- **FR-019**: The extension MUST NOT require any network connectivity and MUST NOT persist audio, transcript content, or dictation history (privacy: the indicator shows state and level, never content).

#### Platform, accessibility & packaging

- **FR-020**: The extension MUST declare the GNOME Shell versions it supports (target Ubuntu 26.04+, GNOME 48/49) and MUST NOT attempt to load on unsupported Shell versions.
- **FR-021**: The extension MUST re-initialize cleanly across Shell restart/session relogin and MUST release all actors, timers, and D-Bus subscriptions on disable (no leaks).
- **FR-022**: State changes MUST be exposed to assistive technologies (screen-reader perceivable) and the indicator MUST remain legible in high-contrast/accessibility modes.
- **FR-023**: On GNOME, this extension becomes the preferred activity-indicator surface; `myna-desktop`'s own indicator MUST remain the fallback when the extension is absent, and enabling the extension MUST NOT change commit-only injection behavior.

### Key Entities *(include if feature involves data)*

- **Dictation state**: the current lifecycle state consumed by the indicator — one of idle, loading/preparing, recording, transcribing, finalizing, error — plus an optional error message; the sole driver of the indicator's visual treatment.
- **Audio level**: a bounded energy/level value (e.g. RMS and/or peak, normalized) published during recording; drives the VU/glow representation and carries no transcript content.
- **Dictation control interface**: the session-bus D-Bus contract exposed by `myna-desktop` — state property, state-change signal, audio-level values, and optional start/stop/toggle commands and error message — that this feature defines and the extension consumes.
- **Indicator surface**: the compositor-hosted, focus-safe visual element (the top-bar "goop"/hanging element and optional panel presence) that renders state and level.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: While the indicator is visible during an active session, keyboard focus never leaves the user's focused application: typed input continues to land in that application in 100% of trials.
- **SC-002**: A person shown the indicator in each state can correctly identify whether the system is loading, listening, transcribing, finalizing, or errored, without seeing any transcript — distinct treatments for all six states (including idle=hidden) are verifiable.
- **SC-003**: The indicator becomes visible within the activation-latency target (≈100–200 ms on reference hardware) after a session starts and clears within the teardown target after it ends.
- **SC-004**: The audio-level representation tracks captured level (rises with louder input, falls to floor on silence) and decays to floor within a short bounded window when the level stream goes stale, in 100% of trials.
- **SC-005**: No transcript content or dictation history is ever rendered by, logged by, or persisted by the extension, and no audio is captured by it (verifiable by inspection and behavior).
- **SC-006**: With `myna-desktop` absent, then started, then stopped, the extension stays dormant, activates, and returns to dormant respectively — with no user-facing errors for these expected conditions and no leaked actors/timers after disable.
- **SC-007**: Animations sustain a smooth frame rate (target ≈60 fps on reference hardware) during each state's animation and do not accumulate overlapping animations across rapid start/stop cycles.
- **SC-008**: The extension loads and functions on the targeted GNOME Shell versions (Ubuntu 26.04+, GNOME 48/49) and refuses to load on unsupported versions rather than failing at runtime.
- **SC-009**: Indicator state changes are announced to a screen reader and the indicator remains legible in high-contrast mode, verifiable with assistive-technology tooling.
- **SC-010**: When a panel trigger is provided, clicking it starts and stops a session equivalently to the hotkey, with identical commit-only text behavior, in 100% of trials.

## Assumptions

- **UI-only scope**: the extension is pure visual feedback plus optional start/stop triggers. Microphone capture, inference orchestration, and IBus text injection stay in `myna-desktop` (feature 003) and are toolkit-agnostic; the shell's direct Clutter text access is not used for injection because it would regress coverage for non-Clutter toolkits (Qt/Electron/Firefox).
- **Single extension**: one GNOME Shell extension (landscape "Option A"), not a separate injection extension.
- **D-Bus contract owned here, implemented in `myna-desktop`**: this feature defines the session-bus interface (state + state-change signal + audio-level values + optional start/stop/toggle + error message); the emitting side is added to `myna-desktop`. Exact member names/signatures are a design detail resolved in planning, guided by the landscape's `org.myna.Dictation` sketch.
- **State vocabulary maps to the internal contract**: idle/loading(preparing)/recording/transcribing/finalizing/error map onto the project's session/liveness phases (`transcription.progress` phases `preparing`/`ready`/`transcribing`, plus finalize/error). Unknown values degrade to neutral (FR-008).
- **Preferred surface on GNOME**: on GNOME this extension is the preferred indicator surface and satisfies feature 003's FR-020 fallback expectation; `myna-desktop`'s own notification/OSD indicator remains the fallback when the extension is not installed/enabled. Other desktops (wlroots/KDE) keep the notification path and are out of scope here.
- **Target platform**: Ubuntu Desktop on Wayland with GNOME 48/49 (Ubuntu 26.04+); older GNOME and non-GNOME desktops are out of scope.
- **Privacy**: consistent with the project invariants — no audio persisted, no transcription content logged/rendered by default; the indicator shows state and level only.
- **Timing targets**: activation-latency and teardown targets are inherited from feature 003 / UD129 (≈100–200 ms activation on reference hardware).
- **Visual/animation design specifics** (exact "goop" geometry, animation family, whether a panel icon is always visible, VU representation, theming, packaging/distribution) are intentionally left as design decisions for planning; the requirements above bound them (focus-safe, state-legible, smooth, privacy-preserving, HIG-compliant) without fixing a single look.
- **Extension language/runtime**: GNOME Shell extensions are GJS/Clutter/St by platform necessity; this is a platform constraint of the compositor, not a violation of the project's Rust-for-shipped-components rule (an in-compositor UI cannot be Rust). To be recorded in the plan's Complexity Tracking.

## Out of Scope

- Text injection of any kind (stays in `myna-desktop` via IBus); using the shell's Clutter text access to commit text.
- A settings panel for model / microphone / language selection or an enable toggle (future feature).
- Support for GNOME Shell versions before 48, and for non-GNOME desktops (wlroots/KDE keep the notification indicator).
- Wake-word / always-on presence, continuous dictation, voice commands, translation, dictation history, transcript display, or audio retention.
- Owning residency/idle-unload policy, model selection, or backend discovery (consumed, not decided, here).
