# Feature Specification: Switchable Basic Dictation HUD

**Feature Branch**: `009-switchable-basic-hud`

**Created**: 2026-07-31

**Status**: Draft

**Input**: User description: "Add a switchable basic GNOME-style dictation HUD alongside the existing wave-ribbon HUD. Users choose their HUD style through a persistent GNOME extension preference. Only one HUD is active at a time. The basic HUD is the default. The basic HUD resembles GNOME's standard audio OSD, with a microphone icon, content-free status, and an input-energy progress bar."

## Clarifications

### Session 2026-07-31

- Q: Which monitor should display the HUD in a multi-monitor session? → A: Always use the primary monitor.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Use a simple native-style dictation indicator (Priority: P1)

A person starts dictation and sees a compact, familiar indicator at the bottom center of the screen. It contains a microphone icon, the current dictation status, and a horizontal bar that confirms how much microphone energy is being detected without the visual complexity of the wave-ribbon presentation.

**Why this priority**: The simpler presentation is the primary user value of the feature and becomes the default experience for people who have not chosen another style.

**Independent Test**: With no HUD preference previously saved, start a dictation session and verify that the basic HUD appears, identifies the current state without transcript content, responds to microphone energy while recording, and never takes focus from the active application.

**Acceptance Scenarios**:

1. **Given** a person has never selected a HUD style, **When** dictation starts, **Then** the basic HUD is the only dictation HUD shown.
2. **Given** the basic HUD is visible and recording, **When** microphone input energy rises or falls, **Then** its horizontal bar rises or falls smoothly within its fixed bounds.
3. **Given** the basic HUD is receiving silence or no fresh level updates, **When** the stale interval elapses, **Then** its bar decays smoothly to empty rather than freezing at the last value.
4. **Given** the basic HUD is visible, **When** the dictation lifecycle changes, **Then** its content-free status identifies loading, listening, transcribing, finalizing, a recoverable notice, or a critical error as applicable.
5. **Given** the basic HUD is visible over a focused application, **When** the person continues typing, **Then** keyboard focus and typed input remain in that application.

---

### User Story 2 - Choose a preferred HUD style (Priority: P1)

A person can choose either the basic HUD or the existing wave-ribbon HUD in the extension's preferences. Their choice is retained across extension disable/enable cycles and desktop sessions, and takes effect immediately without requiring a Shell restart.

**Why this priority**: Coexistence is an explicit feature requirement. Without a durable, user-controlled choice, adding a second presentation would either remove the existing experience or expose an undocumented developer switch.

**Independent Test**: Select each HUD style in turn, verify the visible indicator changes immediately while preserving the current state and level, then restart the extension and confirm the last selection remains active.

**Acceptance Scenarios**:

1. **Given** the basic HUD is selected, **When** the person selects the wave-ribbon HUD, **Then** the basic HUD is removed and the wave-ribbon HUD becomes the only active presentation without a Shell restart.
2. **Given** the wave-ribbon HUD is selected, **When** the person selects the basic HUD, **Then** the wave-ribbon HUD is removed and the basic HUD becomes the only active presentation without a Shell restart.
3. **Given** a dictation session is active when the style changes, **When** the replacement HUD appears, **Then** it reflects the current dictation state and most recent input-energy level without restarting or interrupting dictation.
4. **Given** a person has selected a style, **When** the extension or desktop session restarts, **Then** the selected style remains active.
5. **Given** no valid saved preference is available, **When** the extension starts, **Then** it safely selects the basic HUD.

---

### User Story 3 - Receive equivalent lifecycle and error feedback (Priority: P2)

A person receives the same state, recoverable-notice, and critical-error behavior regardless of which HUD style they choose. Switching styles changes presentation only; it does not change dictation behavior, error persistence, or privacy.

**Why this priority**: A visual preference must not create a functionally weaker or less safe dictation experience. This parity protects the established indicator contract while allowing visual choice.

**Independent Test**: Drive both HUD styles through every supported lifecycle and severity state and verify equivalent status meaning, notice timing, critical-error dismissal, focus safety, and content-free output.

**Acceptance Scenarios**:

1. **Given** either HUD style is active, **When** loading, recording, transcribing, or finalizing begins, **Then** the HUD communicates the same content-free state meaning.
2. **Given** either HUD style is active, **When** a recoverable notice occurs, **Then** it remains non-blocking and clears automatically according to the existing hold behavior.
3. **Given** either HUD style is active, **When** a critical error occurs, **Then** it remains visible until explicitly dismissed and its dismiss action does not take keyboard focus.
4. **Given** either HUD style is active, **When** no dictation session or held notice is present, **Then** no dictation HUD remains visible.
5. **Given** a style change occurs repeatedly, **When** the prior presentation is replaced, **Then** no duplicate HUD, stale timer, or abandoned interactive control remains.

### Edge Cases

- A preference changes before the dictation service becomes available: the selected style is retained and used when service state first arrives.
- A preference changes while the HUD is hidden: no HUD appears solely because of the preference change; the new style is used at the next visible state.
- A preference changes during recording: the replacement reflects the current state and latest known level without interrupting capture, transcription, or injection.
- A preference changes while a recoverable notice is counting down: the replacement notice keeps the remaining hold behavior and does not restart or extend it merely because the style changed.
- A preference changes while a critical error is held: the replacement remains persistent and dismissible; switching style does not dismiss the error.
- Rapid repeated preference changes: only the last selected style remains active, with no accumulated actors, timers, callbacks, or subscriptions.
- A saved preference is absent, malformed, or from an unsupported future value: the basic HUD is selected without surfacing an error to the user.
- Level values are missing, repeated, outside their expected bounds, or stale: the bar remains bounded, accepts repeated fresh updates, and decays safely to empty.
- The dictation service disappears during a style switch: an ordinary active state clears to dormant, while an already-established held notice remains independent of service availability. A recoverable notice expires on its original deadline and a critical error remains until explicitly dismissed; service loss and style switching restart neither lifetime.
- In a multi-monitor session, both HUD styles appear on the primary monitor; switching styles never moves the HUD to another monitor.
- Reduced-motion or high-contrast preferences are enabled: both HUD styles preserve the established accessibility behavior, and the basic energy bar remains legible without decorative motion.

## Requirements *(mandatory)*

### Functional Requirements

#### HUD choice and persistence

- **FR-001**: The system MUST offer exactly two dictation HUD styles: `basic` and `wave ribbon`.
- **FR-002**: The system MUST allow a person to select either style through the dictation extension's user preferences.
- **FR-003**: The system MUST retain the selected style across extension disable/enable cycles and desktop sessions.
- **FR-004**: The system MUST use the basic HUD when no valid preference has been saved.
- **FR-005**: The system MUST display at most one dictation HUD at any time.
- **FR-006**: A style change MUST take effect without requiring a Shell restart and MUST NOT restart, cancel, delay, or otherwise alter the active dictation session.
- **FR-007**: When a style changes during a visible state, the replacement HUD MUST render the current state and latest known input-energy level; when the indicator is idle, changing style MUST NOT make a HUD appear.
- **FR-008**: Replacing a HUD MUST fully release the prior presentation's visual elements, timers, transitions, callbacks, and interactive controls.
- **FR-009**: The wave-ribbon HUD's established geometry, palette, phase animation, and reduced-motion rendering MUST remain unchanged except for becoming selectable rather than the sole presentation. Feature 009's shared controller supersedes its former view-local notice ownership and service-loss lifetime.

#### Basic HUD presentation

- **FR-010**: The basic HUD MUST present as a compact pill at the bottom center of the primary monitor, visually consistent with standard desktop audio indicators without invoking or modifying the desktop's private volume indicator.
- **FR-011**: The basic HUD MUST contain a microphone icon, a content-free dictation status, and a bounded horizontal input-energy bar.
- **FR-012**: The basic HUD MUST support loading, recording/listening, transcribing, finalizing, recoverable-notice, and critical-error states, plus hidden idle behavior.
- **FR-013**: During recording, the energy bar MUST respond smoothly and monotonically to normalized microphone energy: a higher fresh level MUST never produce a lower target fill than a lower fresh level.
- **FR-014**: The energy bar MUST remain between empty and full for every input, including missing, malformed, or out-of-range values.
- **FR-015**: During silence, stale input, and every non-recording lifecycle state, the energy bar MUST decay smoothly to empty rather than freeze or continue representing old audio activity.
- **FR-016**: Repeated fresh level updates with the same numeric values MUST refresh the bar's freshness interval and MUST NOT be mistaken for a stalled stream.
- **FR-017**: The basic HUD MUST communicate lifecycle and severity through content-free icon, label, and visual treatment; it MUST NOT display transcript text.

#### Behavioral parity and safety

- **FR-018**: Both HUD styles MUST consume the same current lifecycle, severity, reason, and normalized level information and MUST assign them the same user-facing meaning.
- **FR-019**: Both HUD styles MUST preserve the existing recoverable-notice behavior: non-blocking display, automatic dismissal, replacement behavior, and ability to start a new session immediately.
- **FR-020**: Both HUD styles MUST preserve the existing critical-error behavior: persistent display until explicit dismissal, replacement behavior, and a pointer-reactive dismiss control that cannot receive keyboard focus.
- **FR-021**: Neither HUD style nor style switching MUST take keyboard focus from the person's active application.
- **FR-022**: Both HUD styles MUST preserve dormant, service-appeared, unknown-state, rapid-state-change, reduced-motion, and high-contrast behavior, and MUST appear on the primary monitor in multi-monitor sessions. When the service vanishes, an ordinary active state MUST clear to dormant, while an already-established recoverable or critical held notice MUST retain its original deadline or dismissal requirement without restart.
- **FR-023**: The feature MUST NOT capture audio, transcribe speech, inject text, require network connectivity, persist audio or transcription content, or expose transcription content through preferences or indicator state.

### Key Entities

- **HUD style preference**: The person's persistent choice between `basic` and `wave ribbon`; absent or invalid values resolve to `basic`.
- **Active HUD**: The single presentation currently responsible for rendering dictation state and level; it may be replaced without changing the dictation session.
- **Basic HUD**: The native-style compact presentation containing a microphone icon, content-free status, and horizontal input-energy bar.
- **Wave-ribbon HUD**: The existing animated presentation retained as the alternative style.
- **Input-energy level**: The bounded, content-free measure of current microphone energy used for visual feedback; it contains neither audio samples nor transcript content.
- **Held notice**: The existing recoverable or critical status whose lifetime is independent of HUD style and therefore survives presentation replacement according to its original timing and dismissal rules.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With no saved choice, the basic HUD is selected in 100% of fresh-install and invalid-preference trials.
- **SC-002**: A person can select either HUD style and see the selected presentation take effect within 250 ms, without restarting the desktop environment or interrupting an active dictation session, in 100% of trials on reference hardware.
- **SC-003**: Across extension and desktop-session restarts, the last valid style choice is restored in 100% of trials.
- **SC-004**: At every observation point during normal operation and rapid style switching, no more than one dictation HUD is visible.
- **SC-005**: For a fixed set of rising and falling microphone-energy inputs, the basic bar produces bounded, monotonic target fills and decays to empty within 600 ms after silence or stale input in 100% of trials.
- **SC-006**: Testers can correctly identify loading, listening, transcribing, finalizing, recoverable notice, and critical error from either HUD without seeing transcript content in at least 90% of state-identification trials.
- **SC-007**: Keyboard focus remains in the previously focused application in 100% of trials covering HUD appearance, style switching, recoverable notices, and pointer dismissal of a critical error.
- **SC-008**: After 100 consecutive style changes, exactly one active presentation remains and no retired presentation reacts to subsequent state, level, preference, or dismiss events.
- **SC-009**: Both styles pass the same lifecycle and severity acceptance suite, including notice replacement, recoverable auto-dismissal, critical persistence, service disappearance, unknown state, and idle hiding.
- **SC-010**: Inspection and runtime tests confirm that neither style renders, logs, transmits, or persists transcript content or raw audio, and the feature performs its core function with network access unavailable.

## Assumptions

- The existing dictation state, severity, level, and user-visible notice behavior are authoritative. Feature 009 changes where notice lifetime is owned and explicitly defines service-loss behavior in FR-022.
- Held-notice lifetime is independent of service availability once established: recoverable notices retain their original deadline and critical errors retain their explicit-dismiss requirement.
- A style preference change takes effect immediately. If a HUD is visible, it is replaced in place while preserving state, latest level, and held-notice lifetime; if hidden, the choice applies to the next visible state.
- The basic HUD becomes the default for both new and existing installations that have no explicit style choice; the current wave-ribbon HUD remains available by selecting it.
- "GNOME-style" means a custom presentation visually aligned with standard desktop audio indicators, not reuse of a private or unrelated system volume indicator.
- The input-energy bar uses the existing normalized level information and calibration; this feature does not change microphone capture or define a new audio metric.
- Screen-reader announcements of state transitions remain tracked separately by global plan task T56. This feature preserves current accessibility semantics but does not close that task.
- Settings for meter sensitivity, colors, geometry, or custom themes are out of scope; the only new user preference is HUD style.
- Changes to the dictation publisher or its state-and-level contract are out of scope because both required inputs already exist.
