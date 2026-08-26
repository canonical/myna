# Feature Specification: GNOME Shell Extension for Myna Dictation UI

**Feature Branch**: `004-gnome-shell-indicator`

**Created**: 2026-07-21

**Status**: Draft

**Input**: User description: "Design and implement a GNOME Shell extension for Ubuntu 26.04+ (GNOME 48/49) that provides a stunning, animated visual interface for the Myna dictation system. The extension solves the Wayland focus-stealing problem that prevents client-side overlays on GNOME by running inside the compositor. Core experience: a 'goop' UI element hanging from the top bar, visible during dictation, alive with a Gemini-Live-style activity pulse, audio-level feedback, and color-coded model states. The extension is pure UI — it visualizes state exposed by the existing myna-desktop process over D-Bus and optionally provides start/stop triggers, but does not perform transcription or injection itself." **(2026-08-26 revision)**: the "runs inside the compositor" framing is superseded — the indicator is now drawn by a standalone GTK4 application whose window the extension hosts as a focus-safe overlay (see Clarifications, 2026-08-26).

## Clarifications

### Session 2026-08-26 (architecture change — renderer moves to a GTK4 application; extension becomes the overlay host)

- Q: The HUD pill is currently drawn by the extension itself (St/Clutter actors, Cairo fallback on Shell 50) — does that stay? → A: No. The presentation moves into a standalone **GTK4 application** (Rust, `myna-hud`) that renders the same bottom-center HUD pill, with the wave ribbon rendered via the **GPU (GLSL) path only**; the Cairo rasterizer and the Shell-50 Cairo fallback tier are deleted outright, not kept as alternates. The GNOME Shell extension remains installed, but as a **thin host**: it launches the application, identifies its window, and manages it as an overlay — it draws nothing.
- Q: How can a normal client window be focus-safe on GNOME, when that was the very reason for drawing inside the compositor? → A: The extension launches the app through the compositor's own Wayland-client API (so it structurally knows which window is its child) and re-types that window as a dock-type, always-above, all-workspaces surface that never takes focus on map and is hidden from window lists; the app itself declares an **empty input region** so pointer events pass through to whatever is underneath. Together these preserve every user-visible guarantee of FR-001 (never steals keyboard focus, never blocks the app being typed into) with the drawing outside the Shell process. The prior claim "on GNOME a normal client *cannot* show an always-on-top, non-focus-stealing overlay" remains true for an **unassisted** client — this design assists the client with a minimal, window-management-only extension.
- Q: What happens to the dismiss (×) control if the window takes no input? → A: The app varies its input region per state: empty (fully click-through) in every state except a critical error, where the region covers only the dismiss control's rectangle. Clicking it still never moves keyboard focus (FR-007c unchanged).
- Q: Why move rendering out of the compositor? → A: One rendering path instead of two rasterizers kept in pixel-lockstep; the renderer becomes a shipped **Rust** component (the project's language rule) instead of harness-tier GJS; the same application serves as the development/tuning lab and the backend simulator **without a running GNOME Shell**; and the accent color comes from the platform's own style/color machinery instead of hand-rolled GSettings tables.
- Q: Does the extension still consume `org.myna.Dictation`? → A: No. The renderer application becomes the consumer of the existing dictation D-Bus interface (unchanged contract). The extension's only bus surface is a new, member-less **presence name** (`org.myna.Shell`) it owns while enabled, so `myna-desktop` can tell whether the hosted indicator exists and suppress its own notification fallback accordingly.
- Q: What about desktops that are not GNOME? → A: The same application can be launched directly by `myna-desktop` where the platform allows a focus-safe overlay (the layer-shell protocol on wlroots/KDE). This change delivers the **policy and contract** for that path (presence watch, well-known binary, same D-Bus consumption) but not the non-GNOME overlay backend itself — that stays follow-up work, now a backend swap behind the same contracts rather than a new feature.
- Q: Does the application run all the time? → A: While the extension is enabled it starts the app once and supervises it: automatic restart (bounded backoff) if it exits unexpectedly, terminate on disable. The app shows no window while idle — push-to-talk (FR-002) is unchanged.
- Q: Where do the developer tuning tool and the backend simulator live now? → A: In the same application, as modes: a lab mode with manual controls (state/severity/level/reduced-motion, dictation target) driving the *identical* renderer with no backend required, and a simulator mode that serves the dictation D-Bus interface so the real hosted indicator can be driven without `myna-desktop`. The old GJS `dev-lab/` and Python `dev-lab-gpu/` tools are deleted.
- Q: Does this change the privacy posture? → A: No. State and level only, on either side of the D-Bus seam; the app renders/logs/persists no transcript; the presence name carries no data at all.

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

### Session 2026-07-30 (wave-ribbon meter — replaces segmented VU meter)

- Q: Should the segmented/discrete VU meter (the prior HUD redesign's R16) be replaced outright or kept as a selectable alternate? → A: Replaced outright. There is no user-facing choice between meter styles; a flowing wave-ribbon becomes the only audio-level representation.
- Q: What form should the audio-level representation take instead of discrete bars? → A: A flowing, organic "wave ribbon" — a small number of translucent, layered strands animating left-to-right from a single smoothed loudness signal, conveying voice presence/intensity (not a literal frequency spectrum or a rendering of raw audio samples), with distinct behavior across the session lifecycle: a brief unfold on start, continuous flow while speaking, a smooth relax toward a thin idle motion during pauses, and a smooth morph into a simplified processing motion when recording ends.
- Q: What color should the ribbon use? → A: The user's system accent-color preference as the primary color, with a lighter/highlight tone and a darker/complementary translucent secondary tone derived from it for depth. If the user has never actively chosen an accent color — including sitting on the untouched system default, even where that default's name coincides with a color a user could also deliberately pick — the ribbon falls back to a fixed default color instead.
- Q: Does the ribbon change behavior for reduced-motion or lower-power preferences? → A: Yes. When the user's system-wide reduced-motion preference is enabled, the flowing ribbon is replaced by a static level line or a gently-scaling microphone indicator, driven by the same underlying level/state inputs, instead of continuous animation.
- Q: Does this change introduce any new way to develop or tune the animation itself? → A: Yes, as a non-shipped addition: a small standalone developer tool that connects to the same real dictation interface as the extension (so it reacts to genuine live audio and state, not simulated data), purely to speed up iterating on the animation's feel — including a plain focusable text area so a real end-to-end dictation session (through to text injection) can be exercised without a separate target application. It carries no independent user-facing requirements and is not part of the shipped extension.

### Session 2026-07-30 ("fabric in gentle airflow" refinement — not an oscilloscope)

- Q: The first wave-ribbon pass drove the wave shape directly from the live audio envelope — is that the intended feel? → A: No. It read as too literal/technical ("nervous, noisy, an oscilloscope"). The wave representation MUST be a smoothed, controlled interpretation of loudness — responsive enough to reassure the user, but never a literal reproduction of audio energy tick-by-tick. Audio drives the animation's *energy*; the product controls its *shape*.
- Q: Does that change what drives colour or add level zones? → A: No — still no green/amber/red loudness zones, no clipping implication, no continuous colour-by-loudness; Ubuntu orange (or the chosen accent) stays primary throughout, with a brighter/warm highlight at the wave's loudest crests and a darker/translucent secondary tone for depth (FR-010b unchanged).
- Q: Should the ribbon stay hidden during a recoverable issue, as originally built? → A: No — reversed. The ribbon now stays **visible** during a recoverable notice, tinted amber (matching the notice's existing amber treatment) with audio-reactivity paused and a gentle idle pulse, rather than hidden — this reads as "still listening, minor issue" instead of "gone dark." A **critical** error still hides/collapses the ribbon entirely; only the recoverable case changed.
- Q: Are sparse "particle" highlights on strong syllables in scope now? → A: Optional and explicitly NOT built out as visible particles in this pass — the design brief itself cautions that overdoing this reads as a music visualizer. The detection logic exists and is unit-tested as a foundation, but no particle rendering ships yet; a future pass may add it conservatively if desired.

### Session 2026-07-21 (informed defaults; see Assumptions)

- Q: Does this feature include text injection, or is it UI-only? → A: UI-only. IBus injection (feature 003) stays and is toolkit-agnostic; the shell's direct Clutter text access covers only Clutter/GTK widgets and would regress coverage for Qt/Electron/Firefox. The extension visualizes state and optionally triggers start/stop; it never commits text.
- Q: One extension or two (UI vs injection)? → A: One extension, UI only (landscape "Option A"). Injection remains in `myna-desktop`.
- Q: How does the extension learn dictation state and audio levels? → A: Over a session-bus D-Bus interface exposed by `myna-desktop`. The extension is a consumer; the interface contract (state property + change signal, audio-level properties, optional start/stop/toggle methods) is defined by this feature but implemented on the `myna-desktop` side.
- Q: Does the extension replace the feature-003 activity indicator? → A: On GNOME it becomes the preferred indicator surface (FR-020 fallback in feature 003); `myna-desktop`'s own indicator remains the fallback when the extension is absent. Commit-only privacy behavior is unchanged.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - See dictation state without losing focus (Priority: P1) 🎯 MVP

A person is typing in an application, starts a dictation session (via the existing hotkey), and sees a compact HUD pill appear — bottom-center of the screen, in the same spot and style as GNOME's own volume/brightness OSD — that clearly shows the system is listening. It never steals focus from the app they are typing into, so dictation is not broken by the indicator itself. When the session ends, the pill clears.

**Why this priority**: This is the reason the extension exists. On GNOME/Wayland an *unassisted* normal client cannot show an always-on-top, non-focus-stealing overlay; the feature-003 notification indicator is constrained and can steal focus. **(2026-08-26)** The focus problem is solved by the extension *hosting* the indicator application's window as an overlay (dock-typed, never focused, click-through — see FR-001/FR-024/FR-025) rather than by drawing inside the compositor; the user-visible guarantee is identical. With only this story, users get a reliable, focus-safe "dictation is active" signal that the client cannot otherwise provide, presented in a form that reads as native GNOME chrome rather than a bespoke overlay.

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
6. **Given** a non-blocking recoverable notice is showing, **When** the person looks at the level representation, **Then** it remains visible — tinted amber, gently pulsing — rather than disappearing, so the indicator reads as "still listening, minor issue" rather than "gone dark."
7. **Given** a persistent critical-error notice is showing, **When** the person looks at the level representation, **Then** it is hidden, consistent with the error's icon/message replacing it.

---

### User Story 3 - See that my voice is being captured, with a premium feel (Priority: P2)

A person sees real-time, organic feedback that their voice is actually being picked up — a flowing, softly glowing wave rendered in their own accent color, rather than a technical-looking meter — so they know the microphone is working and they are speaking at a usable volume, and experience the indicator as a polished, native part of the desktop rather than an audio-engineering tool. The wave settles to a gentle idle motion during silence or pauses, and morphs smoothly rather than cutting abruptly when the session moves between listening, transcribing, and finishing.

**Why this priority**: Level feedback answers the most common failure ("is it hearing me?") and makes the UI feel alive and premium rather than utilitarian. It builds on US1/US2 (the indicator must already exist and show state) and requires an audio-level stream that may not be present in the very first slice, hence P2.

**Independent Test**: With a session active, feed known audio levels through the interface and assert the indicator's flowing level representation tracks them (grows fuller/brighter with louder input, relaxes toward a thin idle motion with silence) at a smooth, responsive update rate, shows no level when idle, transitions smoothly across the session's start/pause/stop, is rendered in the user's accent color (or a default when none is actively chosen), and falls back to a static representation when reduced motion is enabled.

**Acceptance Scenarios**:

1. **Given** an active recording session, **When** the captured audio level rises, **Then** the indicator's flowing level representation grows fuller and brighter, smoothly and within a fixed visual cap (never so bright or large that it becomes distracting).
2. **Given** an active recording session, **When** input goes quiet or pauses, **Then** the representation relaxes smoothly toward a thin, gently moving line rather than stopping abruptly, and a subtle traveling motion may remain to show listening is still active.
3. **Given** no session is active, **When** idle, **Then** no level representation is displayed.
4. **Given** the interface stops publishing level updates (stale), **When** updates lapse beyond a short window, **Then** the representation decays to its floor rather than freezing at the last value.
5. **Given** a dictation session starts, **When** the indicator first appears, **Then** the level representation unfolds smoothly over a brief, sub-second period rather than appearing instantly at full form.
6. **Given** a dictation session ends and moves into transcribing, **When** that transition happens, **Then** the level representation morphs smoothly into a simplified processing motion rather than switching abruptly.
7. **Given** the indicator is rendered, **When** the person has actively chosen a system accent color, **Then** the level representation is rendered in that color; **When** they have not (including the untouched system default), **Then** it renders in a fixed default color instead.
8. **Given** the person has enabled a system-wide reduced-motion preference, **When** the indicator is shown, **Then** the level representation presents as a static or minimally-animated alternative instead of the flowing wave, while still conveying the same state/level information.
9. **Given** a dictation session completes successfully, **When** the HUD pill is about to clear, **Then** the level representation briefly shows a quiet success indication before fading, without delaying the pill's dismissal or blocking a new session from starting.

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

- **`myna-desktop` absent or interface unavailable at load**: the indicator stays dormant (no overlay, no user-facing errors); activates when the interface appears (US1-5).
- **`myna-desktop` disappears mid-session** (crash / disconnect): indicator clears to idle rather than freezing in an active state.
- **Renderer application exits unexpectedly while the extension is enabled** **(2026-08-26)**: it is restarted automatically with a bounded backoff; the indicator reappears when ready; the extension never surfaces an error dialog for this.
- **Extension disabled / Shell logs out** **(2026-08-26)**: the renderer process is terminated; no orphaned overlay window survives the extension.
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
- **System accent color not actively chosen, or unsupported on an older GNOME version**: the level representation falls back to a fixed default color rather than failing or rendering unstyled (US3-7).
- **Reduced-motion preference enabled**: the level representation uses a static/minimal-motion alternative instead of the flowing animation, while still reflecting level and state (US3-8).
- **A recoverable notice is showing while a session is (or was) active**: the level representation stays visible, tinted amber and gently pulsing, rather than disappearing (US2a-6); a critical error, by contrast, hides it (US2a-7).

## Requirements *(mandatory)*

### Functional Requirements

#### Overlay & focus safety

- **FR-001**: The system MUST present a dictation indicator that never takes keyboard focus from the user's focused application at any point in the session lifecycle. **(2026-08-26)** Realized as the window of a standalone renderer application, hosted by the GNOME Shell extension as an overlay (FR-024/FR-025) — no longer drawn inside the compositor; the guarantee itself is unchanged.
- **FR-002**: The indicator MUST be visible during an active dictation session and MUST clear when the session ends, is cancelled, or errors out — there MUST be no persistent overlay while idle (push-to-talk).
- **FR-003**: The indicator MUST appear within the activation-latency target after a session starts and MUST clear within the teardown target after it ends (consistent with feature 003 timing targets).
- **FR-004**: The indicator MUST present as a compact pill positioned bottom-center of the screen — matching the position and general presentation of GNOME's own volume/brightness OSD, not the top-of-panel placement of the prior "goop" design — and MUST render correctly in single- and multi-monitor layouts without appearing off-screen. **(2026-08-26)** The pill is the renderer application's window; the extension positions it bottom-center of the primary monitor's work area and re-positions it on monitor/work-area/size changes.
- **FR-024** **(2026-08-26)**: While hosted, the indicator MUST NOT behave as a normal application window: it MUST have no taskbar/alt-tab/window-list entry, MUST be shown on all workspaces, MUST stay above normal application windows, and MUST NOT be movable, minimizable, or closable by ordinary window-management means.
- **FR-025** **(2026-08-26)**: The indicator MUST be click-through — pointer events MUST pass through it to the application underneath — in every state, with the sole exception of the critical-error dismiss (×) control (FR-007c), whose interactive area is limited to that control's rectangle.
- **FR-026** **(2026-08-26)**: While the extension is enabled, the renderer application MUST be supervised: started automatically, restarted automatically after an unexpected exit (with a bounded, non-aggressive backoff), and terminated on extension disable. The user must never need to relaunch the indicator manually.
- **FR-027** **(2026-08-26)**: The renderer application MUST be launchable as a well-known binary (`myna-hud`) from standard locations, so the extension can start it on GNOME and the desktop client can start it elsewhere, without a discovery protocol.

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

- **FR-010**: The indicator MUST provide a real-time audio-level representation as a flowing, organic wave (not a discrete segmented meter) during recording, updated at a smooth, responsive rate, and calibrated so ordinary conversational speech is clearly visible rather than requiring an elevated voice. The wave MUST be a **smoothed, controlled interpretation** of loudness, not a literal, tick-by-tick reproduction of the audio envelope — it MUST NOT read as an oscilloscope, frequency display, or other audio-engineering instrument.
- **FR-010a**: The wave representation MUST unfold smoothly over a brief (sub-second) period when a session starts, MUST relax smoothly toward a thin, minimally-animated idle motion during pauses/silence rather than stopping abruptly, and MUST morph smoothly into a simplified processing motion when the session moves from recording into transcribing, rather than switching treatments abruptly.
- **FR-010b**: The wave representation MUST be rendered using the user's system accent-color preference as its primary color (with a lighter/highlight tone and a darker/complementary translucent secondary tone derived from it for depth — the darker tone is a computed complement of the primary color, **except when the primary color is orange, where it is a fixed aubergine tone** rather than a generic computed complement), and MUST fall back to a fixed default color when the user has not actively chosen an accent color — an untouched system default MUST be treated as not actively chosen, even where its name coincides with a color a user could also deliberately select — or when the system does not support an accent-color preference at all.
- **FR-010c**: The wave representation's brightness and size MUST be capped so that even the loudest input never renders in a way that is visually distracting or overwhelms the surrounding indicator chrome.
- **FR-010d**: When a session completes successfully, the wave representation MUST briefly show a quiet success indication before the HUD pill clears, and this MUST NOT delay the user's ability to start a new session.
- **FR-010e**: During a **recoverable** notice, the wave representation MUST remain visible — tinted to match the notice's amber treatment, with audio-reactivity paused (a gentle idle pulse instead of tracking live input) — rather than hidden. During a **critical** error, the wave representation MUST be hidden, consistent with the persistent error notice and mic-with-slash icon replacing it.
- **FR-011**: The audio-level representation MUST show no level while idle and MUST decay toward its floor when the level stream goes stale or silent (never freeze at the last value).
- **FR-012**: The audio-level representation MUST convey only level/energy and MUST NOT render or leak any transcript content.

#### Panel presence & triggers

- **FR-013**: The system MAY provide a panel presence (tray/top-bar button); if present, it MUST follow GNOME Human Interface Guidelines for panel buttons and be subtle/non-intrusive.
- **FR-014**: If a panel trigger is provided, it MUST allow the user to start and stop/toggle a dictation session, equivalent in effect to the existing hotkey activation, preserving commit-only behavior.
- **FR-015**: When a trigger command is unavailable, the extension MUST give non-intrusive feedback and MUST NOT leave a stuck visual state.

#### Integration with myna-desktop (D-Bus)

- **FR-016**: The indicator MUST obtain dictation state and audio levels from the existing `myna-desktop` process over a session-bus D-Bus interface, and MUST NOT capture audio, perform transcription, or inject text itself. **(2026-08-26)** The consumer is now the renderer application (previously the extension); the contract itself is unchanged.
- **FR-017**: The D-Bus interface MUST expose, at minimum: the current dictation state, a state-change notification, audio-level values, and a content-free severity classification (recoverable vs. critical) for the error state; and MAY expose start/stop/toggle commands and an error message. The interface contract is defined by this feature and implemented on the `myna-desktop` side. The severity classification is an interim, client-inferred value (e.g. empty-transcript-on-finalize → recoverable, all other terminal errors → critical) pending a future wire-level disposition (T31/T62); it is additive and MUST NOT change the meaning of the existing terminal-error behavior for clients that don't read it.
- **FR-017a** **(2026-08-26)**: The extension MUST make its presence discoverable as a well-known session-bus name (`org.myna.Shell`) owned for exactly as long as it is enabled and able to host the indicator. The name carries no properties, methods, or signals — ownership itself is the signal. The desktop client MUST use it to choose the indicator surface: name present → the hosted renderer is the indicator and the client's own fallback indicator is suppressed; name absent → the existing notification fallback applies.
- **FR-018**: The indicator MUST tolerate `myna-desktop` being absent at load, appearing later, and disappearing mid-session: it stays dormant when the interface is unavailable, activates when it appears, and clears to idle if it disappears — without surfacing errors to the user for these expected conditions. **(2026-08-26)** "The indicator" here is the renderer application (previously the extension).
- **FR-019**: The indicator MUST NOT require any network connectivity and MUST NOT persist audio, transcript content, or dictation history (privacy: the indicator shows state and level, never content).

#### Platform, accessibility & packaging

- **FR-020**: The extension MUST declare the GNOME Shell versions it supports (target Ubuntu 26.10+, GNOME 50/51) and MUST NOT attempt to load on unsupported Shell versions.
- **FR-021**: The extension MUST re-initialize cleanly across Shell restart/session relogin and MUST release all actors, timers, subprocesses, and D-Bus subscriptions (including the presence name) on disable (no leaks). **(2026-08-26)** now includes terminating the hosted renderer process.
- **FR-022**: The indicator MUST remain legible in high-contrast/accessibility modes. (Screen-reader/AT-SPI announcement of state transitions is tracked separately as T56 and is out of scope for this change.)
- **FR-022a**: The indicator MUST honor the user's system-wide reduced-motion preference: when enabled, the flowing wave representation MUST be replaced by a static or minimally-animated alternative that still conveys state and level, rather than continuing full animation.
- **FR-023**: On GNOME, the extension-hosted renderer is the preferred activity-indicator surface; `myna-desktop`'s own indicator MUST remain the fallback when the extension is absent, and enabling the extension MUST NOT change commit-only injection behavior. **(2026-08-26)** Preferred/absent is detected via FR-017a's presence name; `myna-desktop`'s old experimental GTK overlay indicator is removed (superseded by the renderer application).

### Key Entities *(include if feature involves data)*

- **Dictation state**: the current lifecycle state consumed by the indicator — one of idle, loading/preparing, recording, transcribing, finalizing, error — plus, for the error state, a severity classification (recoverable | critical) and an optional content-free reason; the sole driver of the indicator's visual treatment.
- **Audio level**: a bounded energy/level value (RMS and peak, normalized) published during recording; drives the flowing wave representation (or its static reduced-motion alternative) and carries no transcript content.
- **Dictation control interface**: the session-bus D-Bus contract exposed by `myna-desktop` — state property, state-change signal, audio-level values, error severity classification, and optional start/stop/toggle commands and error message — that this feature defines and the renderer application consumes. **(2026-08-26)** consumer changed from the extension to the renderer application; contract unchanged.
- **Shell presence** **(2026-08-26)**: the session-bus name owned by the extension while it is enabled and able to host the indicator; watched by the desktop client to select the active indicator surface. Carries no data.
- **Hosted overlay window** **(2026-08-26)**: the renderer application's toplevel window while hosted on GNOME — dock-typed, hidden from window lists, on all workspaces, always above normal windows, click-through except for the critical-error dismiss control, positioned bottom-center of the primary monitor's work area by the extension.
- **Renderer application** **(2026-08-26)**: the standalone GUI application (`myna-hud`) that draws the indicator — pill, status, severity treatments, and the wave ribbon (GPU-rendered) — consumes the dictation control interface, and additionally provides the development lab and backend-simulator modes.
- **Indicator surface**: the user-visible indicator: the hosted overlay window (bottom-center, OSD-styled) and the optional panel presence that render state, severity, and level.
- **Accent color preference**: the user's system-wide accent-color choice, or its absence, used to color the wave representation; sourced from the desktop environment itself (via the platform's style/color machinery), not from `myna-desktop` or the dictation session.
- **Motion preference**: the user's system-wide reduced-motion setting, used to choose between the flowing wave and its static/minimal-motion alternative.

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
- **SC-011**: The audio-level visualization is rendered in the user's actively-chosen system accent color, or a fixed default color when none has been actively chosen, in 100% of trials — verifiable across at least three different chosen accent colors plus the untouched-default case.
- **SC-012**: When a person has enabled their system's reduced-motion preference, the audio-level visualization presents its static/minimal-motion alternative instead of the flowing animation in 100% of trials, while still correctly reflecting level and state.
- **SC-013**: In a structured side-by-side comparison with at least 3 observers, a majority describe the flowing wave representation as smoother and more polished than the discrete meter it replaces.
- **SC-014**: During a recoverable notice, the level representation remains visible (amber, gently pulsing) in 100% of trials rather than disappearing; during a critical error, it is hidden in 100% of trials.
- **SC-015** **(2026-08-26)**: While the indicator is visible, clicks on it (outside the critical-error dismiss control) pass through to the application underneath in 100% of trials; the indicator never appears in the taskbar/alt-tab/window list, appears on every workspace, and stays above normal windows, on single- and multi-monitor layouts.
- **SC-016** **(2026-08-26)**: If the renderer application exits unexpectedly while the extension is enabled, it is restarted automatically within a short bounded window and the indicator returns without user action, in 100% of trials; disabling the extension terminates it in 100% of trials.

## Assumptions

- **UI-only scope**: the feature is pure visual feedback plus optional start/stop triggers. Microphone capture, inference orchestration, and IBus text injection stay in `myna-desktop` (feature 003) and are toolkit-agnostic. **(2026-08-26)** The extension is now pure *window management* (it hosts and positions the renderer's window; it draws nothing and carries no dictation data); the renderer application is the pure-UI half. Neither captures audio, transcribes, or injects text.
- **Renderer application** **(2026-08-26)**: the indicator is a standalone GTK4 + libadwaita application (`myna-hud`, Rust), rendering the wave ribbon via the GPU shader path only — the Cairo rasterizer and the Shell-50 Cairo fallback are deleted outright, not retained behind a flag. The same binary provides the development lab and backend-simulator modes (non-shipped capabilities of a shipped binary).
- **Single extension**: one GNOME Shell extension (landscape "Option A"), the thin overlay host; no separate injection extension.
- **D-Bus contract owned here, implemented in `myna-desktop`**: this feature defines the session-bus interface (state + state-change signal + audio-level values + error severity + optional start/stop/toggle + error message); the emitting side is added to `myna-desktop`. Exact member names/signatures are a design detail resolved in planning, guided by the landscape's `org.myna.Dictation` sketch. **(2026-08-26)** plus the member-less presence name `org.myna.Shell` (FR-017a).
- **State vocabulary maps to the internal contract**: idle/loading(preparing)/recording/transcribing/finalizing/error map onto the project's session/liveness phases (`transcription.progress` phases `preparing`/`ready`/`transcribing`, plus finalize/error). Unknown values degrade to neutral (FR-008).
- **Error severity is an interim, client-inferred signal, not a wire-level disposition**: recoverable-vs-critical is classified by `myna-desktop` today from the coarse signal available (an empty/zero-length committed transcript on finalize → recoverable; every other terminal error → critical). This is a stopgap ahead of T31/T62's proper error-taxonomy work landing severity on the wire itself; this feature does not attempt to build that taxonomy.
- **Preferred surface on GNOME**: on GNOME the extension-hosted renderer is the preferred indicator surface and satisfies feature 003's FR-020 fallback expectation; `myna-desktop`'s notification indicator remains the fallback when the extension is not installed/enabled, selected via the presence name (FR-017a). Other desktops keep the notification path; the future layer-shell overlay backend for wlroots/KDE is contract-ready behind the same seams but out of scope this pass.
- **Supervision model** **(2026-08-26)**: one renderer process per enabled extension, started at `enable()`, kept alive with bounded-backoff respawn, terminated at `disable()`; not started per-session (the app hides its window while idle rather than exiting).
- **Well-known binary** **(2026-08-26)**: `myna-hud` is resolved from a small fixed order of standard locations (packaged snap command, system path, developer override); no discovery protocol.
- **Target platform**: Ubuntu Desktop on Wayland with GNOME 50/51 (Ubuntu 26.10+); older GNOME and non-GNOME desktops are out of scope.
- **Privacy**: consistent with the project invariants — no audio persisted, no transcription content logged/rendered by default; the indicator shows state and level only. The presence name carries no data.
- **Timing targets**: activation-latency and teardown targets are inherited from feature 003 / UD129 (≈100–200 ms activation on reference hardware); the recoverable-notice auto-dismiss delay reuses the existing hold window already used by the prior implementation (≈3.5s) rather than introducing a new tunable.
- **Visual/animation design specifics** (exact pill geometry, icon set beyond the mic/mic-slash distinction, wave strand/control-point counts, exact accent-color derivation, packaging/distribution) are intentionally left as design decisions for planning; the requirements above bound them (focus-safe, state-legible, smooth, privacy-preserving, HIG-compliant, bottom-center OSD-styled) without fixing every pixel.
- **Extension language/runtime**: **(2026-08-26, superseded in part)** the *renderer* is now Rust (`myna-hud`), aligning with the project's Rust-for-shipped-components rule. The *extension host* remains GJS — GNOME Shell extensions are GJS/Clutter/Meta by platform necessity — but it is now a thin window-management shim (launch, adopt, position, supervise; no drawing, no dictation data), a much smaller carve-out than the previous full-renderer-in-GJS one. Recorded in the plan's Complexity Tracking.
- **Custom widget, not Shell's internal OSD class**: the HUD pill is styled to resemble GNOME's OSD (in the renderer application's own toolkit), not a reuse of Shell's internal `OsdWindow` implementation — avoiding a dependency on private Shell UI internals that are not a stable extension API.
- **Prior goop implementation removed, not retained**: the Cairo/`RibbonView` presentation is deleted once the HUD view lands; it is not kept as a selectable alternate view. **(2026-08-26)** likewise the extension-side HUD renderer (St/Clutter actors, Cairo and GLSL paths) and the GJS/Python dev labs are deleted outright once the GTK renderer lands.
- **T56 and US4 are unaffected**: screen-reader/AT-SPI announcements (T56) remain separate, unspecced future work; the optional panel click-to-toggle affordance (US4) is untouched by this redesign.
- **No coordination with GNOME's native OSD**: incidental simultaneous on-screen display with GNOME's own volume/brightness OSD (both occupy the bottom-center region) is acceptable; this feature does not implement collision-avoidance, suppression, or repositioning logic to coordinate with it.
- **Accent-color and reduced-motion are desktop-environment preferences, not new settings this feature introduces**: both are read from GNOME's existing system-wide preferences. **(2026-08-26)** both are now sourced inside the renderer application via the platform's own machinery (the libadwaita style/color manager plus the standard interface settings), since the application no longer runs inside the Shell; the FR-010b untouched-default fallback rule is unchanged.
- **Localization** **(2026-08-26)**: status strings are translatable via gettext under the project-wide `myna` domain (previously the extension's own domain), with the translation catalog moving next to the renderer application.
- **The wave stays a synthesized envelope, not raw audio**: consistent with the project's audio-in-UI privacy posture (no samples, no waveform of actual audio), the flowing wave is driven by the same single smoothed loudness value the segmented meter used — never raw PCM — so this redesign does not reopen the earlier rejection of a literal waveform on privacy grounds.
- **A developer lab and simulator ship inside the renderer binary** **(2026-08-26, supersedes the prior standalone dev-lab assumption)**: lab mode (manual controls + dictation target, no backend required) and simulator mode (serves `org.myna.Dictation`) are modes of `myna-hud` driving the identical renderer modules. They are development aids, not user-facing surfaces, and carry no independent functional requirements beyond behaving identically to the shipped rendering paths.
- **Sparse particle highlights are deferred, not required**: the "fabric in gentle airflow" refinement's optional 4th layer (brief highlight points on strong syllables) is intentionally not rendered in this pass — only the underlying detection is built, as a foundation for a future, conservative addition if desired. The design brief itself cautions that overdoing this reads as a music visualizer; omitting the rendering is a deliberate scope choice, not an oversight.

## Out of Scope

- Text injection of any kind (stays in `myna-desktop` via IBus); using the shell's Clutter text access to commit text.
- A settings panel for model / microphone / language selection or an enable toggle (future feature).
- A destination for the critical-error notice's dismiss control beyond clearing the notice itself — it is not a link to any settings, help, or troubleshooting surface (none is being designed here).
- A true wire-level error disposition/taxonomy (T31/T62) — this feature only consumes an interim, client-inferred severity classification.
- Screen-reader/AT-SPI announcements of state transitions (T56) — tracked separately.
- Support for GNOME Shell versions before 50, and for non-GNOME desktops (wlroots/KDE keep the notification indicator). **(2026-08-26)** the non-GNOME overlay backend (layer-shell) is *contract-ready* behind the same seams — this change delivers the presence/launch policy and the well-known binary, but not the backend itself.
- Client-side (non-extension) launching of the renderer beyond the documented policy/watch — including any D-Bus activation.
- Wake-word / always-on presence, continuous dictation, voice commands, translation, dictation history, transcript display, or audio retention.
- Owning residency/idle-unload policy, model selection, or backend discovery (consumed, not decided, here).
- A user-facing choice between meter styles (segmented vs. wave) — the wave representation fully replaces the segmented meter with no alternate; likewise a user-facing choice between rasterizers (the GPU path is the only one).
- Public distribution of the extension or renderer beyond the project's own packaging channels (in-tree install and the myna snap; store/EGO review remains follow-up, as before).
