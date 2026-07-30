# Feature Specification: GNOME Shell Extension for Myna Dictation UI

**Feature Branch**: `004-gnome-shell-indicator`

**Created**: 2026-07-21

**Status**: Draft

**Input**: User description: "Design and implement a GNOME Shell extension for Ubuntu 26.04+ (GNOME 48/49) that provides a stunning, animated visual interface for the Myna dictation system. The extension solves the Wayland focus-stealing problem that prevents client-side overlays on GNOME by running inside the compositor. Core experience: a 'goop' UI element hanging from the top bar, visible during dictation, alive with a Gemini-Live-style activity pulse, audio-level feedback, and color-coded model states. The extension is pure UI — it visualizes state exposed by the existing myna-desktop process over D-Bus and optionally provides start/stop triggers, but does not perform transcription or injection itself."

## Clarifications

### Session 2026-07-30 (HUD redesign — goop replaced)

- Q: Should the goop/Cairo view be removed or kept as an alternate view? → A: Replaced. The `RibbonView` implementation is removed; a new HUD-style view is the only indicator surface. The `view.js` `IndicatorView` seam (interface + factory) is unchanged, so this is a swap of the implementation behind an already-stable contract, not a rearchitecture.
- Q: Which lifecycle states does the HUD cover? → A: All six from the existing state model (idle=hidden, loading, recording, transcribing, finalizing, error) — not narrowed to only the states pictured in the reference design. Loading/transcribing/finalizing reuse today's existing status copy verbatim (see states.js), restyled into the new pill.
- Q: The reference design shows two visually distinct problem states ("Recoverable issue" vs "Critical error") but the wire only carries one terminal `error`/`transcription.error` today (T31's severity-on-the-wire work is not done) — where does the split come from? → A: Best-effort now, client-side. `myna-desktop` classifies a session that finalizes with an empty/zero-length committed transcript as the **recoverable** tier (e.g. "no speech detected"); every other `transcription.error` stays the **critical** tier. True wire-level disposition remains T31/T62's job; this is an interim, inferred classification the extension consumes over the same D-Bus interface (an added boolean/enum field, not a new error taxonomy).
- Q: How does a recoverable issue clear, versus a critical error? → A: Recoverable issues auto-dismiss after the same hold window the current indicator already uses (3.5s), during which the user can immediately retry (push-to-talk is unaffected — nothing blocks a new session starting while the pill is still showing). Critical errors are persistent until the user dismisses them with an explicit close (×) control on the pill; they never auto-dismiss.
- Q: Is that close control a link to a settings/help surface (as its chevron-like appearance in the reference design might suggest)? → A: No — it is a dismiss (×) button only. Clicking it hides the pill and clears the held error; it does not open any settings, help, or troubleshooting surface. No such surface is being designed as part of this change.
- Q: Does the dismiss button conflict with the indicator's non-focus-stealing invariant (FR-001)? → A: No. Only the dismiss control is pointer-reactive (`reactive: true`); it remains non-focusable (`can_focus: false`) like the rest of the chrome, so a mouse click can dismiss it without ever taking keyboard focus.
- Q: Does the level meter stay a continuous animated shape (the goop's flowing blob), or something else? → A: A segmented/discrete bar meter (a fixed set of vertical bars whose heights track the live normalized audio level), not a continuous waveform or blob.
- Q: Does the mic icon change per state? → A: Yes — a filled microphone icon for recording and other non-error states, and a microphone-with-slash icon specifically for the critical-error tier (e.g. "Microphone unavailable"). The recoverable tier keeps the plain filled icon since the microphone itself is not the fault.
- Q: Where does the HUD sit on screen? → A: Bottom-center of the screen, matching the position of GNOME's own volume/brightness OSD — not the goop's top-of-panel placement — and sized as a narrow pill rather than the goop's ~80%-monitor-width ribbon.
- Q: Does this redesign also close T56 (screen-reader/AT-SPI announcements) since the view is being rebuilt anyway? → A: No. T56 stays separate, unspecced future work; this change is visual/interaction only.
- Q: Does this redesign touch US4 (the optional panel click-to-toggle affordance)? → A: No. US4 is unaffected and remains a separate, independent panel presence from the HUD pill.

### Session 2026-07-30 (clarify pass)

- Q: If a second critical error arrives while a first critical-error notice is still undismissed, what happens? → A: Replace in place — the notice updates to the new error's reason/icon; the still-undismissed notice's persistence carries over (it still requires an explicit dismiss, the replacement does not restart or waive that requirement).
- Q: If a second recoverable issue arrives while a first recoverable notice is still auto-dismissing, what happens? → A: Replace in place and restart the auto-dismiss countdown — the notice updates to the new occurrence's reason and gets a fresh full-length hold window, rather than clearing on the original's now-stale schedule.
- Q: Since the HUD now sits in the same screen region as GNOME's native volume/brightness OSD, should the extension actively avoid/coordinate with that overlap? → A: No special handling required. Incidental simultaneous display (e.g. the user adjusts volume while dictating) is acceptable; whichever ordering the Shell's own chrome stacking produces is fine — no collision-avoidance or repositioning logic is required.

### Session 2026-07-21 (informed defaults; see Assumptions)

- Q: Does this feature include text injection, or is it UI-only? → A: UI-only. IBus injection (feature 003) stays and is toolkit-agnostic; the shell's direct Clutter text access covers only Clutter/GTK widgets and would regress coverage for Qt/Electron/Firefox. The extension visualizes state and optionally triggers start/stop; it never commits text.
- Q: One extension or two (UI vs injection)? → A: One extension, UI only (landscape "Option A"). Injection remains in `myna-desktop`.
- Q: How does the extension learn dictation state and audio levels? → A: Over a session-bus D-Bus interface exposed by `myna-desktop`. The extension is a consumer; the interface contract (state property + change signal, audio-level properties, optional start/stop/toggle methods) is defined by this feature but implemented on the `myna-desktop` side.
- Q: Does the extension replace the feature-003 activity indicator? → A: On GNOME it becomes the preferred indicator surface (FR-020 fallback in feature 003); `myna-desktop`'s own indicator remains the fallback when the extension is absent. Commit-only privacy behavior is unchanged.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See dictation state without losing focus (Priority: P1) 🎯 MVP

A person is typing in an application, starts a dictation session (via the existing hotkey), and sees a compact HUD pill appear — bottom-center of the screen, in the same spot and style as GNOME's own volume/brightness OSD — that clearly shows the system is listening. It never steals focus from the app they are typing into, so dictation is not broken by the indicator itself. When the session ends, the pill clears.

**Why this priority**: This is the reason the extension exists. On GNOME/Wayland a normal client cannot show an always-on-top, non-focus-stealing overlay; the feature-003 notification indicator is constrained and can steal focus. Running inside the compositor eliminates the focus problem and is the minimum that delivers value. With only this story, users get a reliable, focus-safe "dictation is active" signal that the client cannot otherwise provide, presented in a form that reads as native GNOME chrome rather than a bespoke overlay.

**Independent Test**: With the extension installed and `myna-desktop` running, start a dictation session while a text field is focused; assert the indicator becomes visible, that keyboard focus remains in the original text field (typing still lands there), and that the indicator clears when the session ends.

**Acceptance Scenarios**:

1. **Given** the extension is installed and `myna-desktop` is running, **When** a dictation session starts, **Then** a HUD pill becomes visible bottom-center of the screen within the activation-latency target.
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

### User Story 2a - Tell a passing hiccup from a real problem (Priority: P1)

A person dictates and nothing was heard (they spoke too quietly, or paused too long before the mic picked anything up) — a brief, self-clearing notice tells them to just try again, and they can start a new session immediately without any extra step. Separately, when something is actually broken (no microphone available, the backend is unreachable), a persistent notice stays on screen with a clear reason, until the person dismisses it themselves.

**Why this priority**: Not every wire-level `transcription.error` means the same thing to the user. Treating a harmless "didn't catch that" the same as "your microphone is broken" either trains people to ignore real failures (if everything auto-dismisses) or nags them with a dismiss click for a trivial, self-explanatory miss (if nothing auto-dismisses). This distinction is core to the redesign's legibility goal, so it ships with the MVP rather than as a later refinement.

**Independent Test**: Drive `myna-desktop` through (a) a session that finalizes with an empty transcript and (b) a session that hits a hard failure (e.g. simulated microphone-unavailable); assert (a) shows a brief, auto-clearing notice that a new session can start over during or immediately after, and (b) shows a persistent notice with a dismiss control that only clears on explicit user action.

**Acceptance Scenarios**:

1. **Given** a dictation session finalizes with no committed transcript (nothing was heard), **When** that result reaches the indicator, **Then** a non-blocking notice appears (e.g. "No speech detected — try speaking again") and clears on its own after a short, bounded delay.
2. **Given** the non-blocking notice is showing, **When** the person starts a new dictation session, **Then** the new session proceeds normally and is not blocked or delayed by the still-visible notice.
3. **Given** a hard failure occurs (e.g. no microphone available), **When** that error reaches the indicator, **Then** a persistent notice appears with a clear, content-free reason and a visible dismiss control, and it does NOT clear on its own.
4. **Given** a persistent notice is showing, **When** the person activates its dismiss control, **Then** the notice clears immediately and does not reappear on its own.
5. **Given** a persistent notice's dismiss control, **When** the person points at or activates it with the mouse, **Then** keyboard focus never leaves the user's currently focused application (the dismiss control is clickable but never focusable, consistent with FR-001).

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
- **A recoverable-issue notice is still showing when a new session starts**: the new session proceeds unaffected; the two are independent (US2a-2).
- **A critical-error notice's dismiss (×) control is activated with the mouse**: keyboard focus never moves to it (US2a-5); it is pointer-reactive but never focusable.
- **A second critical error arrives before the first is dismissed**: the notice updates in place to the new reason; it does not stack, queue, or waive the dismiss requirement (FR-007d).
- **A second recoverable issue arrives before the first has auto-dismissed**: the notice updates in place to the new occurrence and the auto-dismiss delay restarts in full (FR-007a).
- **The HUD pill and GNOME's native volume/brightness OSD appear at the same time**: no collision-avoidance is required; incidental simultaneous display is acceptable and whichever stacking order the Shell's chrome layer produces stands.
- **GNOME Shell version mismatch**: extension declares its supported Shell versions and does not attempt to load on unsupported versions (see Assumptions).
- **Shell lock/restart** (`Alt+F2 r` on X11 is unavailable on Wayland; session relogin): extension re-initializes cleanly and reconnects to the interface; no leaked actors or timers.
- **Rapid state churn** (fast start/stop): indicator does not accumulate overlapping animations or leak actors; transitions coalesce.
- **High-contrast / accessibility mode**: indicator remains legible; state changes are perceivable by assistive technology.
- **Multi-monitor / panel on a specific monitor**: indicator positions bottom-center consistently and does not appear off-screen.

## Requirements *(mandatory)*

### Functional Requirements

#### Overlay & focus safety

- **FR-001**: The system MUST present a dictation indicator that runs inside the GNOME Shell compositor and MUST NOT take keyboard focus from the user's focused application at any point in the session lifecycle.
- **FR-002**: The indicator MUST be visible during an active dictation session and MUST clear when the session ends, is cancelled, or errors out — there MUST be no persistent overlay while idle (push-to-talk).
- **FR-003**: The indicator MUST appear within the activation-latency target after a session starts and MUST clear within the teardown target after it ends (consistent with feature 003 timing targets).
- **FR-004**: The indicator MUST present as a compact pill positioned bottom-center of the screen — matching the position and general presentation of GNOME's own volume/brightness OSD, not the top-of-panel placement of the prior "goop" design — and MUST render correctly in single- and multi-monitor layouts without appearing off-screen.

#### State visualization

- **FR-005**: The indicator MUST show visually distinct treatments for each dictation state: idle (hidden), loading/preparing (model load in progress), recording/listening, transcribing, finalizing, error.
- **FR-006**: The model-loading/preparing state MUST be visually distinct from the listening state so a cold-model load is legible as "loading" and not mistaken for "listening".
- **FR-007**: The indicator MUST distinguish two severities of problem: a **recoverable** issue (e.g. no speech detected in the session) and a **critical** error (e.g. microphone unavailable, backend unreachable). A recoverable issue MUST render as a non-blocking, auto-clearing notice; a critical error MUST render as a persistent notice that remains until the user dismisses it (see FR-007a–FR-007c). Both MUST surface a clear, content-free reason.
- **FR-007a**: A recoverable-issue notice MUST clear on its own after a short, bounded delay (no user action required) and MUST NOT block or delay a new dictation session from starting while it is still visible. If a new recoverable issue arrives while one is already showing, the notice MUST update in place to the new occurrence and the auto-dismiss delay MUST restart in full (not continue on the original's schedule).
- **FR-007b**: A critical-error notice MUST remain visible until the user explicitly dismisses it via a dedicated dismiss control, and MUST NOT auto-clear.
- **FR-007c**: The critical-error notice's dismiss control MUST be pointer-reactive (clickable) but MUST NOT be keyboard-focusable, so dismissing it can never take keyboard focus from the user's focused application (consistent with FR-001).
- **FR-007d**: If a new critical error arrives while a critical-error notice is already showing and undismissed, the notice MUST update in place to the new error's reason (replacing the prior one) rather than stacking or queuing multiple notices; the replacement MUST still require an explicit dismiss (it MUST NOT restart as auto-dismissing and MUST NOT count as already dismissed).
- **FR-008**: The indicator MUST degrade gracefully when it receives an unrecognized state value, falling back to a neutral active/idle treatment rather than breaking.
- **FR-009**: Animations MUST be smooth and MUST NOT block or visibly stutter the compositor; animations MUST stop and their resources be released when the session clears (no accumulation across rapid start/stop cycles).

#### Audio-level feedback

- **FR-010**: The indicator MUST provide a real-time audio-level representation as a segmented VU meter (a fixed set of discrete segments that illuminate left-to-right, colour-zoned green/yellow/red by position) during recording, updated at a smooth, responsive rate, and calibrated so ordinary conversational speech is clearly visible rather than requiring an elevated voice.
- **FR-011**: The audio-level representation MUST show no level while idle and MUST decay toward its floor when the level stream goes stale or silent (never freeze at the last value).
- **FR-012**: The audio-level representation MUST convey only level/energy and MUST NOT render or leak any transcript content.

#### Panel presence & triggers

- **FR-013**: The system MAY provide a panel presence (tray/top-bar button); if present, it MUST follow GNOME Human Interface Guidelines for panel buttons and be subtle/non-intrusive.
- **FR-014**: If a panel trigger is provided, it MUST allow the user to start and stop/toggle a dictation session, equivalent in effect to the existing hotkey activation, preserving commit-only behavior.
- **FR-015**: When a trigger command is unavailable, the extension MUST give non-intrusive feedback and MUST NOT leave a stuck visual state.

#### Integration with myna-desktop (D-Bus)

- **FR-016**: The extension MUST obtain dictation state and audio levels from the existing `myna-desktop` process over a session-bus D-Bus interface, and MUST NOT capture audio, perform transcription, or inject text itself.
- **FR-017**: The D-Bus interface MUST expose, at minimum: the current dictation state, a state-change notification, audio-level values, and a content-free severity classification (recoverable vs. critical) for the error state; and MAY expose start/stop/toggle commands and an error message. The interface contract is defined by this feature and implemented on the `myna-desktop` side. The severity classification is an interim, client-inferred value (e.g. empty-transcript-on-finalize → recoverable, all other terminal errors → critical) pending a future wire-level disposition (T31/T62); it is additive and MUST NOT change the meaning of the existing terminal-error behavior for clients that don't read it.
- **FR-018**: The extension MUST tolerate `myna-desktop` being absent at load, appearing later, and disappearing mid-session: it stays dormant when the interface is unavailable, activates when it appears, and clears to idle if it disappears — without surfacing errors to the user for these expected conditions.
- **FR-019**: The extension MUST NOT require any network connectivity and MUST NOT persist audio, transcript content, or dictation history (privacy: the indicator shows state and level, never content).

#### Platform, accessibility & packaging

- **FR-020**: The extension MUST declare the GNOME Shell versions it supports (target Ubuntu 26.10+, GNOME 50/51) and MUST NOT attempt to load on unsupported Shell versions.
- **FR-021**: The extension MUST re-initialize cleanly across Shell restart/session relogin and MUST release all actors, timers, and D-Bus subscriptions on disable (no leaks).
- **FR-022**: The indicator MUST remain legible in high-contrast/accessibility modes. (Screen-reader/AT-SPI announcement of state transitions is tracked separately as T56 and is out of scope for this change.)
- **FR-023**: On GNOME, this extension becomes the preferred activity-indicator surface; `myna-desktop`'s own indicator MUST remain the fallback when the extension is absent, and enabling the extension MUST NOT change commit-only injection behavior.

### Key Entities *(include if feature involves data)*

- **Dictation state**: the current lifecycle state consumed by the indicator — one of idle, loading/preparing, recording, transcribing, finalizing, error — plus, for the error state, a severity classification (recoverable | critical) and an optional content-free reason; the sole driver of the indicator's visual treatment.
- **Audio level**: a bounded energy/level value (RMS and peak, normalized) published during recording; drives the segmented, colour-zoned VU meter and carries no transcript content.
- **Dictation control interface**: the session-bus D-Bus contract exposed by `myna-desktop` — state property, state-change signal, audio-level values, error severity classification, and optional start/stop/toggle commands and error message — that this feature defines and the extension consumes.
- **Indicator surface**: the compositor-hosted, focus-safe HUD pill (bottom-center, OSD-styled) and optional panel presence that renders state, severity, and level.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: While the indicator is visible during an active session, keyboard focus never leaves the user's focused application: typed input continues to land in that application in 100% of trials, including when a person dismisses a critical-error notice with the mouse.
- **SC-002**: A person shown the indicator in each state can correctly identify whether the system is loading, listening, transcribing, finalizing, has hit a recoverable issue, or has hit a critical error, without seeing any transcript — distinct treatments for all seven states/severities (including idle=hidden) are verifiable.
- **SC-003**: The indicator becomes visible within the activation-latency target (≈100–200 ms on reference hardware) after a session starts and clears within the teardown target after it ends.
- **SC-004**: The audio-level representation tracks captured level (rises with louder input, falls to floor on silence) and decays to floor within a short bounded window when the level stream goes stale, in 100% of trials.
- **SC-005**: No transcript content or dictation history is ever rendered by, logged by, or persisted by the extension, and no audio is captured by it (verifiable by inspection and behavior).
- **SC-006**: With `myna-desktop` absent, then started, then stopped, the extension stays dormant, activates, and returns to dormant respectively — with no user-facing errors for these expected conditions and no leaked actors/timers after disable.
- **SC-007**: Animations sustain a smooth frame rate (target ≈60 fps on reference hardware) during each state's animation and do not accumulate overlapping animations across rapid start/stop cycles.
- **SC-008**: The extension loads and functions on the targeted GNOME Shell versions (Ubuntu 26.10+, GNOME 50/51) and refuses to load on unsupported versions rather than failing at runtime.
- **SC-009**: A recoverable-issue notice clears on its own within a short, bounded delay in 100% of trials without any user action, and never blocks a subsequent session from starting; a critical-error notice persists until dismissed in 100% of trials and never clears on its own.
- **SC-010**: When a panel trigger is provided, clicking it starts and stops a session equivalently to the hotkey, with identical commit-only text behavior, in 100% of trials.

## Assumptions

- **UI-only scope**: the extension is pure visual feedback plus optional start/stop triggers. Microphone capture, inference orchestration, and IBus text injection stay in `myna-desktop` (feature 003) and are toolkit-agnostic; the shell's direct Clutter text access is not used for injection because it would regress coverage for non-Clutter toolkits (Qt/Electron/Firefox).
- **Single extension**: one GNOME Shell extension (landscape "Option A"), not a separate injection extension.
- **D-Bus contract owned here, implemented in `myna-desktop`**: this feature defines the session-bus interface (state + state-change signal + audio-level values + error severity + optional start/stop/toggle + error message); the emitting side is added to `myna-desktop`. Exact member names/signatures are a design detail resolved in planning, guided by the landscape's `org.myna.Dictation` sketch.
- **State vocabulary maps to the internal contract**: idle/loading(preparing)/recording/transcribing/finalizing/error map onto the project's session/liveness phases (`transcription.progress` phases `preparing`/`ready`/`transcribing`, plus finalize/error). Unknown values degrade to neutral (FR-008).
- **Error severity is an interim, client-inferred signal, not a wire-level disposition**: recoverable-vs-critical is classified by `myna-desktop` today from the coarse signal available (an empty/zero-length committed transcript on finalize → recoverable; every other terminal error → critical). This is a stopgap ahead of T31/T62's proper error-taxonomy work landing severity on the wire itself; this feature does not attempt to build that taxonomy.
- **Preferred surface on GNOME**: on GNOME this extension is the preferred indicator surface and satisfies feature 003's FR-020 fallback expectation; `myna-desktop`'s own notification/OSD indicator remains the fallback when the extension is not installed/enabled. Other desktops (wlroots/KDE) keep the notification path and are out of scope here.
- **Target platform**: Ubuntu Desktop on Wayland with GNOME 50/51 (Ubuntu 26.10+); older GNOME and non-GNOME desktops are out of scope.
- **Privacy**: consistent with the project invariants — no audio persisted, no transcription content logged/rendered by default; the indicator shows state and level only.
- **Timing targets**: activation-latency and teardown targets are inherited from feature 003 / UD129 (≈100–200 ms activation on reference hardware); the recoverable-notice auto-dismiss delay reuses the existing hold window already used by the prior implementation (≈3.5s) rather than introducing a new tunable.
- **Visual/animation design specifics** (exact pill geometry, icon set beyond the mic/mic-slash distinction, bar-meter segment count, theming, packaging/distribution) are intentionally left as design decisions for planning; the requirements above bound them (focus-safe, state-legible, smooth, privacy-preserving, HIG-compliant, bottom-center OSD-styled) without fixing every pixel.
- **Extension language/runtime**: GNOME Shell extensions are GJS/Clutter/St by platform necessity; this is a platform constraint of the compositor, not a violation of the project's Rust-for-shipped-components rule (an in-compositor UI cannot be Rust). To be recorded in the plan's Complexity Tracking.
- **Custom widget, not Shell's internal OSD class**: the HUD pill is a new St-based widget styled to resemble GNOME's OSD, not a reuse of Shell's internal `OsdWindow` implementation — avoiding a dependency on private Shell UI internals that are not a stable extension API.
- **Prior goop implementation removed, not retained**: the Cairo/`RibbonView` presentation (`indicator.js`) is deleted once the new HUD view lands; it is not kept as a selectable alternate view. The `view.js` `IndicatorView` interface and factory (the swap seam) are unchanged.
- **T56 and US4 are unaffected**: screen-reader/AT-SPI announcements (T56) remain separate, unspecced future work; the optional panel click-to-toggle affordance (US4) is untouched by this redesign.
- **No coordination with GNOME's native OSD**: incidental simultaneous on-screen display with GNOME's own volume/brightness OSD (both now occupy the bottom-center region) is acceptable; this feature does not implement collision-avoidance, suppression, or repositioning logic to coordinate with it.

## Out of Scope

- Text injection of any kind (stays in `myna-desktop` via IBus); using the shell's Clutter text access to commit text.
- A settings panel for model / microphone / language selection or an enable toggle (future feature).
- A destination for the critical-error notice's dismiss control beyond clearing the notice itself — it is not a link to any settings, help, or troubleshooting surface (none is being designed here).
- A true wire-level error disposition/taxonomy (T31/T62) — this feature only consumes an interim, client-inferred severity classification.
- Screen-reader/AT-SPI announcements of state transitions (T56) — tracked separately.
- Support for GNOME Shell versions before 48, and for non-GNOME desktops (wlroots/KDE keep the notification indicator).
- Wake-word / always-on presence, continuous dictation, voice commands, translation, dictation history, transcript display, or audio retention.
- Owning residency/idle-unload policy, model selection, or backend discovery (consumed, not decided, here).
