# Feature Specification: Accessible Dictation UX
        
**Featur            e Branch**: `011-accessible-dictation-ux`

**Created**: 2026-08-26

**Status**: Draft

**Input**: Myna is brand new and lacks basic accessibility support. Make the whole dictation experience — activation, live state feedback, and errors — usable by disabled users, across screen-reader, low-vision, motor-impairment, deaf/hard-of-hearing, and cognitive/learning needs.

Dictation is itself an assistive technology: a large share of Myna's users will be people with RSI, limited hand mobility, low vision, dyslexia, or other conditions that make typing costly. Today the product serves them poorly. Everything the user needs to know is delivered **visually** — a bottom-centre HUD pill, a coloured wave ribbon, and desktop notifications — while the GNOME Shell indicator is deliberately non-focusable chrome whose label updates silently, so a screen reader announces nothing as state advances (tracked as T56 in `docs/project-plan.md`, opened 2026-07-20 and still unspecced). There are no non-visual cues of any kind.

This feature makes accessibility a shipped property of the product rather than a backlog item, and establishes the acceptance gates that keep it from regressing. A settings/preferences UI and first-run onboarding are being delivered as a separate feature by a different team; this spec requires that the preferences it depends on (announcement verbosity, sound cues, silence auto-stop) exist and behave correctly, without specifying or building the surface that exposes them.

## Clarifications

### Session 2026-08-26

- Q: Which user groups does this feature serve? → A: All of them — blind/screen-reader, low-vision, motor-impairment/hands-free, deaf/hard-of-hearing, and cognitive/learning. No group is deferred; priorities order the delivery, not the audience.
- Q: Which currently-missing surfaces are in scope? → A: Screen-reader announcements for state changes (both the Shell HUD and the GTK overlay), optional non-visual (sound) cues, and screen-reader-friendly CLI output.
- Q: Is text-injection accessibility (undo, edit-by-voice correction, preedit behaviour under assistive technology) in scope? → A: No — deferred to its own feature. The interaction between in-field unstable hypotheses and screen-reader re-reading is recorded here as a risk and an edge case, but this feature does not change the injection path.
- Q: Is a settings/preferences UI (including first-run onboarding) in scope? → A: No — a separate feature, owned by a different team, is delivering it. This spec requires the underlying preferences to exist and behave correctly (announcement verbosity, sound cues, silence auto-stop) but does not specify or build the surface that exposes them to the user.
- Q: What should announcement verbosity default to before any preference is ever set? → A: All transitions. Anything quieter means a blind user gets no state feedback at all until the separate settings feature ships and they find the control, which would make SC-001 impossible to satisfy out of the box.
- Q: Which failure set does FR-023's plain-language mapping bind to — today's ad-hoc error strings, or the not-yet-built stable taxonomy? → A: Today's ad-hoc error strings (for example in `client/myna-desktop/src/controller.rs`). This feature maps plain language onto whatever failures exist now; if the stable error-code taxonomy (T31) and error-state UX mapping (T62) land later, their failures are adopted compatibly rather than this feature waiting on them.
- Q: If emitting an accessibility announcement itself fails, what must happen to the dictation session? → A: The session continues unaffected, and the announcement failure is itself surfaced as a recoverable notice through the remaining working channels — it is not silently swallowed.
- Q: Is localisation of user-facing strings in scope? → A: No — cut as premature. There is no translation pipeline, no translators, and no locale roadmap for Myna today; building RTL layout and pseudo-translation testing infrastructure ahead of an actual translation effort is speculative scope for an early-stage product. Existing i18n scaffolding in the Shell extension is unaffected and not undone.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — A blind user dictates without looking at the screen (Priority: P1)

A screen-reader user presses their dictation shortcut, hears "listening", speaks, ends the session, and hears "transcribing" and then confirmation that the text was inserted — all without any visual channel. If something goes wrong, they hear what failed and what to do about it. Nothing spoken ever contains transcript content. At any moment they can also query the current dictation state rather than relying on having caught the announcement.

**Why this priority**: This is the largest and most complete failure today. Every state signal the product emits is visual; a blind user cannot tell whether the microphone is live, whether the model is still loading, or whether the session failed. Without this, Myna is unusable by an entire class of its natural users, and it closes the long-standing T56 gap for both indicator implementations.

**Independent Test**: With a screen reader running and the display switched off, complete a full dictation into a text editor and then trigger an error (dictate with the backend stopped). Every state change is spoken within the announcement latency budget, and the failure and its recovery action are understandable without sighted assistance.

**Acceptance Scenarios**:

1. **Given** a screen reader is running, **When** the user starts a dictation session, **Then** the transition to listening is announced within the latency budget, without keyboard focus moving and without the focused application losing focus.
2. **Given** a session is running, **When** the state advances (loading → listening → transcribing → finishing → idle), **Then** each transition the user's verbosity setting includes is announced exactly once, and rapid consecutive transitions are coalesced rather than queued into overlapping speech.
3. **Given** a braille display is in use, **When** state changes occur, **Then** the same content-free state text reaches the braille device through the same accessibility path as speech.
4. **Given** a session fails, **When** the error is surfaced, **Then** the announcement names the failure in plain language and states the next action, and distinguishes a recoverable hiccup from a hard failure without relying on colour or on the visual indicator.
5. **Given** an assistive technology queries the dictation indicator at any moment (not at a transition), **Then** the current state is programmatically determinable with an accessible name and description.
6. **Given** the user has set announcement verbosity to errors-only, **When** a normal session runs, **Then** only failures are announced; **and given** verbosity is set to off, **Then** no announcements are made while the state remains programmatically queryable.

---

### User Story 2 — A user who cannot hold a key chord can still start and stop dictation (Priority: P1)

A user with limited hand mobility — or one driving the desktop through a single switch, head pointer, or on-screen keyboard — starts dictation with one discrete action, speaks for as long as they need, and stops either with a second discrete action or by simply stopping speaking. Nothing requires holding a key down, pressing two keys at once, or hitting a small on-screen target.

**Why this priority**: Motor impairment is the single most common reason people reach for dictation in the first place, and tap-to-toggle activation — already the shipped default on both activation paths — is what serves them; it must never regress toward requiring a held or chorded key. What is genuinely unverified is whether that default holds up under the desktop's own accessibility features (sticky keys, slow keys, autorepeat), whether every acknowledgement path works without a pointer, and whether a session can end with no further input at all — silence auto-stop is tracked separately and is still open.

**Independent Test**: Configure the system so the only available input is a single non-repeating activation event (simulate a switch or a single bound key). Start a session, dictate a multi-sentence utterance, and end it both ways: by a second activation, and by falling silent. Both complete successfully with text inserted; at no point is a held or chorded key required.

**Acceptance Scenarios**:

1. **Given** the shipped default (tap-to-toggle), **When** the user taps the shortcut once, **Then** dictation starts and stays live with no key held; **When** they tap again, **Then** the session ends and the text is inserted.
2. **Given** the user has bound a single, unmodified key (or a hardware dictation key), **When** they activate it, **Then** dictation starts — the system never requires a multi-key chord.
3. **Given** sticky keys or slow keys are enabled in the desktop's accessibility settings, **When** the user activates the shortcut, **Then** activation still registers exactly once and key autorepeat does not produce duplicate sessions.
4. **Given** the user stops speaking, **When** the configured silence interval elapses, **Then** the session ends by itself and the text is inserted — no second input is required to finish.
5. **Given** an error indicator is showing that must be acknowledged, **When** the user has no pointing device, **Then** they can dismiss or resolve it through a non-pointer path; no acknowledgement is pointer-only or hover-only.
6. **Given** the user activates dictation through the desktop's own switch-access, dwell-click, or similar assistive input feature rather than a bound key, **Then** the session starts identically to a shortcut activation — Myna imposes no requirement beyond accepting an ordinary key or pointer event.

---

### User Story 3 — Every state is perceivable in more than one way (Priority: P1)

Whatever a user cannot perceive — colour, motion, small text, sound, or the screen entirely — they still know what dictation is doing. Severity is never carried by colour alone, confirmation is never carried by sound alone, and the indicator stays legible when the desktop is set to large text, high contrast, or reduced motion.

**Why this priority**: This is the cross-cutting rule that keeps every other story honest, and it covers the low-vision and deaf/hard-of-hearing audiences that neither US1 nor US2 reaches. It is also what prevents a well-meaning fix for one group (for example, adding chimes) from creating a new barrier for another.

**Independent Test**: Produce a coverage table of every user-visible state against every feedback channel and confirm each state is carried by at least one visual and one non-visual channel, with no state distinguished by colour alone or sound alone. Then run a session with large text (200%), high contrast, forced colours, and reduced motion all enabled and confirm the indicator remains complete, legible, and unclipped.

**Acceptance Scenarios**:

1. **Given** any state or severity distinction, **When** it is presented, **Then** it is conveyed by text or shape in addition to colour, and by a visible channel in addition to any sound.
2. **Given** the desktop's reduced-motion preference is set, **When** dictation runs, **Then** the level animation is replaced by a static equivalent that still conveys that capture is live, and no element flashes more than three times per second under any setting.
3. **Given** the desktop text scale is increased to 200% or a high-contrast theme is active, **When** the indicator is shown, **Then** all of its text is fully visible without clipping or truncation and meets the contrast thresholds for text and non-text elements.
4. **Given** optional sound cues are enabled, **When** a session starts, ends, or fails, **Then** each cue is distinct and short, is accompanied by an equivalent visual and accessibility-layer signal, and can be disabled independently of every other feedback channel.
5. **Given** sound cues or spoken announcements occur while the microphone is live, **When** the resulting transcript is compared to a silent baseline, **Then** transcription accuracy is not measurably degraded.
6. **Given** the user has disabled the Shell indicator, or the extension is not installed, **When** dictation runs, **Then** a complete non-visual and visual feedback path still exists — no state is observable only through one optional surface.

---

### User Story 4 — Failures explain themselves in plain language (Priority: P2)

Whatever goes wrong — no microphone, permission denied, model not installed, unsupported language, backend unavailable, no text field focused, protected field, nothing heard — the user is told what happened and what to do next, in ordinary words, through whichever channels they can perceive, and identically across the indicator, the notification, the accessibility layer, and the terminal.

**Why this priority**: Error handling is where a cognitive- or learning-disabled user is most likely to be stranded, and where a blind user is most likely to be stuck in an unrecoverable state. It depends on the announcement machinery from US1.

**Independent Test**: Force each failure in the taxonomy and check that every one produces a single plain-language message naming the problem and the next action, that the same meaning appears on every surface, and that recoverable and critical failures are distinguishable without colour.

**Acceptance Scenarios**:

1. **Given** any failure in the taxonomy, **When** it surfaces, **Then** the primary message names what happened and the next action in plain language, without error codes, jargon, or internal component names as the primary text.
2. **Given** the same failure occurs, **When** it is shown on the indicator, in a notification, in the terminal, and through the accessibility layer, **Then** all four convey the same meaning and the same recovery action.
3. **Given** a recoverable notice and a critical failure, **When** each is presented, **Then** the two are distinguishable without relying on colour, and the critical one persists until acknowledged while the recoverable one does not require action.
4. **Given** a notice auto-dismisses, **When** the user missed it, **Then** they can still discover what happened afterwards — the transient presentation is not the only record.
5. **Given** the model is loading or another long operation is under way, **When** it exceeds the expected duration, **Then** the user receives a periodic non-visual indication that work is still progressing and, past a threshold, an actionable message — a non-visual user can always distinguish "working" from "hung".

---

### User Story 5 — The terminal client is readable by a screen reader (Priority: P3)

A user who drives the terminal with a screen reader or braille display runs the command-line dictation client and can follow what is happening: stable, line-oriented output with explicit textual state markers, no meaning carried by colour or emoji alone, and no animated spinner or cursor redrawing that floods speech.

**Why this priority**: The terminal client is primarily a developer and evaluation surface, so it is the lowest-value of the in-scope surfaces — but it is also cheap to fix and is the surface blind developers and testers will actually use to verify the rest of this feature.

**Independent Test**: Run the terminal client under a screen reader with colour disabled and confirm that session state, transcript output, and errors are all understandable from the plain-text stream alone, with no repeated re-reading caused by in-place redrawing.

**Acceptance Scenarios**:

1. **Given** colour and emoji are stripped from the output, **When** a session runs, **Then** state, results, and failures remain fully distinguishable from the text alone.
2. **Given** a screen reader is reading terminal output, **When** the client reports progress, **Then** it does not repeatedly rewrite the same line or emit animation frames that cause repeated re-reading.
3. **Given** a failure occurs, **When** it is reported, **Then** it goes to the error stream in the same plain language used by the graphical surfaces.

---

### Edge Cases

- **Screen-reader speech captured by the live microphone**: announcements and sound cues occur while capture is active; audible output must not contaminate the transcript. This constrains *when* and *how* cues are emitted, and is measured against a silent baseline rather than assumed.
- **Announcement storms**: a session can pass through several states in well under a second (loading → listening → transcribing), and a failing backend can retry. Announcements must be coalesced or superseded so the user is not stuck listening to a stale queue.
- **User misses a transient signal**: a notice that auto-dismisses, or an announcement spoken while the user was away, must not be the only record of what happened.
- **No indicator present**: the Shell extension may be absent, disabled, or the desktop may not be GNOME; notifications may be suppressed by a do-not-disturb mode. A complete feedback path must survive the loss of any single surface.
- **Assistive technology is not running**: announcement machinery must be inert and cost-free when no assistive technology is listening, and must not change behaviour that sighted users depend on.
- **Announcement delivery itself fails**: if the call that emits an accessibility announcement errors (for example, the accessibility bus is unreachable), the dictation session MUST continue completely unaffected — capture, transcription, and injection are never delayed or blocked by it — and the failure MUST itself be surfaced as a recoverable notice through whichever channels are still working, rather than being silently dropped.
- **Confined packaging**: the shipped package is strictly confined; the accessibility path must work under confinement or the feature does not ship — this must be verified in the packaged form, not only in a development build.
- **No keyboard, no pointer**: a user driving the desktop by switch, eye tracking, or on-screen keyboard must have at least one complete activation and acknowledgement path.
- **Extreme scaling and forced colours**: text scale beyond 200%, forced-colour themes, and screen magnification at high zoom must not clip, truncate, or hide state — including the case where the magnified viewport does not contain the indicator's screen position.
- **In-field unstable hypotheses under assistive technology**: streaming hypotheses rendered into the focused field can cause a screen reader to re-read the field on every revision. This feature does not change the injection path — but the risk is recorded, and resolving it is a prerequisite for any future change that makes in-field hypotheses more prominent.
- **Long-running silence auto-stop**: an automatic end-of-session must not surprise a user who was thinking rather than finished; the interval is configurable, and the impending stop is perceivable.
- **Preferences are read by more than one process**: announcement verbosity, sound cues, and silence auto-stop are honoured by separate components (at minimum the desktop daemon and the Shell extension, each a distinct process) that do not today share one preference store. Whichever feature builds the control surface, every component that must honour a preference MUST observe the same value for it — this feature does not specify the storage mechanism, but does depend on that consistency existing, and flags it against the already-tracked open question of which component owns configuration (`docs/project-plan.md` T54).
- **Headless verification of the Shell extension's accessibility tree**: GNOME Shell's nested-compositor mode is unavailable on Wayland (no `gnome-shell --nested`), which limits how much of the Shell HUD's live accessibility exposure can be driven by an automated test without a real compositor session. The state-to-descriptor mapping and the values it would expose can be verified headlessly; whether the full AT-SPI tree can also be asserted headlessly for this specific surface is a planning-phase question, not assumed here.

## Requirements *(mandatory)*

### Functional Requirements

#### Perception through assistive technology

- **FR-001**: Every user-visible dictation state MUST be exposed to assistive technologies with an accessible name and description, and MUST be programmatically determinable on demand — not only at the instant of a transition.
- **FR-002**: State transitions MUST produce a proactive announcement to assistive technologies (reaching both speech and braille through the same path), without moving keyboard focus, without stealing focus from the user's application, and without requiring the user to focus the indicator.
- **FR-002a**: If emitting an announcement fails, the dictation session MUST continue unaffected (capture, transcription, and injection are never delayed or blocked), and the failure MUST be surfaced as a recoverable notice (FR-025) through whichever channels remain working — it MUST NOT be silently swallowed.
- **FR-003**: Announcements MUST be content-free: state, severity, and recovery action only, never transcript text — including never unstable hypotheses (constitution Principle V).
- **FR-004**: Announcement verbosity MUST be user-configurable with at least three levels — off, failures only, and all transitions — MUST default to all transitions before any preference is set, and the chosen level MUST NOT affect the programmatic queryability required by FR-001.
- **FR-005**: Announcements MUST be coalesced or superseded so that a burst of state changes produces at most one current announcement, and a stale announcement is never spoken after the state it describes has passed.
- **FR-006**: Both shipped indicator implementations (the Shell indicator and the overlay presented by the desktop client) MUST satisfy FR-001 through FR-005 identically, so a user's experience does not depend on which indicator is active.
- **FR-007**: The accessibility path MUST function in the strictly confined shipped package, and MUST be inert — imposing no measurable cost — when no assistive technology is listening.

#### Multi-modal redundancy

- **FR-008**: Every user-visible state and severity distinction MUST be conveyed through at least one visual and one non-visual channel; no state may be distinguishable only by sight and none only by sound.
- **FR-009**: No information may be carried by colour alone; every colour distinction MUST be accompanied by text or shape.
- **FR-010**: The system MUST offer optional sound cues for session start, stop listening (the moment capture ends and processing begins, before the outcome is known), session end, and failure. Cues MUST be short, mutually distinct, individually disableable, and always accompanied by equivalent visual and assistive-technology signals.
- **FR-011**: Audible feedback emitted while capture is live MUST NOT measurably degrade transcription accuracy relative to a silent baseline.
- **FR-012**: The system MUST honour the desktop's reduced-motion, high-contrast, forced-colour, text-scale, and accent-colour preferences; with reduced motion set, live capture MUST still be indicated by a static equivalent.
- **FR-013**: Indicator text and non-text elements MUST meet recognised contrast thresholds (at minimum 4.5:1 for text and 3:1 for meaningful non-text elements) in every shipped theme variant, and MUST remain complete and unclipped at up to 200% text scale.
- **FR-014**: No user-facing element may flash or flicker more than three times per second under any setting.

#### Operation without precise or sustained input

- **FR-015**: Activation MUST NOT require holding a key for the duration of an utterance: tap-to-start/tap-to-stop MUST remain available, selectable, and the default (this is a non-regression requirement — it is already the shipped behaviour, and must stay so).
- **FR-016**: Activation MUST be bindable to a single, unmodified key (including a hardware dictation key where present); a multi-key chord MUST NEVER be required.
- **FR-017**: Activation MUST behave correctly under the desktop's own keyboard accessibility features (at minimum sticky keys and slow keys) and MUST NOT produce duplicate sessions under key autorepeat.
- **FR-018**: A session MUST be able to end without further user input, after a configurable interval of silence, and the impending automatic end MUST be perceivable before it happens.
- **FR-019**: Every action the user can take on an indicator or notification — including acknowledging a failure — MUST be available through a non-pointer, non-hover path; no action may be pointer-only.
- **FR-020**: Activation MUST NOT depend on Myna providing its own on-screen or pointer-free control surface: any assistive input method that generates an ordinary key or pointer event (for example, the desktop's own switch-access or dwell-click features) MUST be sufficient to activate dictation with no additional accommodation required.
- **FR-021**: User-facing time limits MUST be configurable and MUST NOT be the only route through an interaction: automatic dismissal of a notice and the silence auto-stop interval MUST each be adjustable or have a non-timed alternative. This excludes brief, non-interactive internal timing (for example, the acquisition wait used to detect a protected field) that is never presented to the user as a limit they are racing against.
- **FR-022**: Dictation MUST NOT move the user's keyboard focus at any point, and no indicator surface may enter the keyboard focus chain.

#### Failure communication

- **FR-023**: Every failure in the product's error taxonomy MUST map to a plain-language message that names what happened and the next action, without error codes, jargon, or internal component names as the primary text.
- **FR-024**: A given failure MUST convey the same meaning and the same recovery action across every surface on which it appears — indicator, notification, terminal, and assistive-technology announcement.
- **FR-024a**: The same underlying concept (a given state, severity, or failure) MUST use identical wording everywhere it is named across surfaces — no surface may introduce a synonym, abbreviation, or alternate phrasing for something another surface already names, so a user learns a state's name once.
- **FR-025**: Recoverable and critical failures MUST be distinguishable without colour and without vision, and a critical failure MUST persist until acknowledged.
- **FR-026**: A transient presentation MUST NOT be the only record of a failure: after any auto-dismissed notice, the user MUST still be able to determine what happened.
- **FR-027**: Long operations — including model loading — MUST emit a periodic non-visual indication of progress, and past a defined threshold MUST surface an actionable message, so a non-visual user can always distinguish work in progress from a hang.

#### Terminal client

- **FR-028**: Terminal output MUST remain fully meaningful with colour and emoji removed, using explicit textual state markers.
- **FR-029**: Terminal output MUST be stable and line-oriented while a screen reader is in use — no in-place redrawing or animation that causes repeated re-reading — and failures MUST go to the error stream in the same plain language used elsewhere.

#### Verification and non-regression

- **FR-030**: The accessibility properties of each indicator — presence, accessible name, description, and state — MUST be verified by automated tests that require neither a physical display nor a human operator. Where a component's full compositor session cannot itself be driven headlessly, the automated test MUST at minimum verify the accessible values that component would expose, and the gap in end-to-end coverage MUST be recorded rather than silently assumed closed.
- **FR-031**: A documented manual acceptance protocol MUST exist covering, at minimum: a complete dictation with the screen off under a screen reader, a complete dictation without a pointing device, and a complete dictation without a sustained or chorded keypress.
- **FR-032**: Automated checks MUST guard against regression in at least: contrast thresholds, colour-only encoding, and the state-to-channel coverage matrix.
- **FR-033**: The accessibility acceptance gates MUST be exercised against the strictly confined packaged build, not only against development builds.

### Key Entities

- **User-visible state**: the set of states the product exposes to a user (idle, loading, listening, transcribing, finishing, notice, failure), each with a content-free label, a severity, and a recovery action where applicable. This is the unit that the coverage matrix and the announcement machinery both operate on.
- **Feedback channel**: a route by which a state reaches a user — visual indicator, notification, sound cue, assistive-technology announcement, programmatic property, terminal text — each classified as visual or non-visual, and each independently disableable.
- **State-to-channel coverage matrix**: the checked-in mapping of every user-visible state to the channels that carry it, with the invariant that each state has at least one visual and one non-visual channel. It is the artefact the automated coverage gate asserts against.
- **Accessibility preference set**: the user's persisted choices governing announcement verbosity, sound cues, and silence auto-stop interval; this spec requires that they exist, persist, and take effect correctly, whichever surface (owned by a separate feature) exposes them to the user.
- **Failure presentation**: a failure's plain-language name, cause, recovery action, and severity — authored once and rendered identically on every channel.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A blind user with a screen reader completes a full dictation — start, speak, stop, confirm insertion — with the display off and without sighted assistance, on first attempt.
- **SC-002**: 100% of user-visible states are carried by at least one visual and at least one non-visual channel, with zero states distinguishable by colour alone or by sound alone, as asserted by the coverage gate.
- **SC-003**: State-change announcements reach assistive technologies within 500 ms of the transition, and no announcement describing a superseded state is ever delivered.
- **SC-004**: A user can start and end a dictation session using only discrete single-key activations, with no key held longer than a normal keypress and no chord — and can end a session with no input at all.
- **SC-005**: The indicator remains complete, unclipped, and above the required contrast thresholds at 200% text scale, in the high-contrast theme, and with reduced motion enabled.
- **SC-006**: Enabling sound cues and announcements changes transcription accuracy on the real corpus by no more than 0.5 percentage points of word error rate relative to the silent baseline.
- **SC-007**: After any failure, a user relying solely on non-visual channels can state what went wrong and what to do next within 10 seconds of the failure, without sighted assistance.
- **SC-008**: Terminal output remains fully interpretable with colour and emoji stripped, and produces no repeated re-reading under a screen reader during a complete session.
- **SC-009**: Every accessibility acceptance gate passes against the strictly confined packaged build, and the automated subset runs in continuous integration on every change, failing the build on regression.

## Assumptions

- The reference assistive-technology stack is the one shipped with the target desktop — the platform screen reader on Wayland, with braille reaching the user through the same accessibility path. Support for other stacks follows from using the platform accessibility layer rather than from targeting each one.
- The guiding rubric is WCAG 2.2 at level AA, adapted to a desktop rather than a web context, alongside the platform's human-interface accessibility guidance. Criteria that are meaningless for a non-document, non-focusable status surface are recorded as not applicable rather than silently dropped.
- Existing product invariants carry unchanged and are not renegotiated here: no transcript text in any indicator, notification, or announcement; no audio persisted; capture only between explicit start and end; the indicator never takes keyboard focus.
- Tap-to-toggle is already the shipped default activation mode on both activation paths (the portal binding and the control-socket binding); hold-to-talk remains available as a launch-time choice. FR-015 is therefore a non-regression requirement protecting existing behaviour, not a new product decision.
- **A settings/preferences UI is a separate, out-of-scope feature owned elsewhere.** This spec does not build, specify the form of, or gate on any settings application, preferences window, or onboarding flow. Wherever this spec requires something to be "configurable" (announcement verbosity — FR-004; sound cues — FR-010; silence auto-stop — FR-018, FR-021), the requirement is on the preference existing, persisting, and taking effect correctly — not on this feature delivering the control surface for it. That other feature is responsible for making whatever surface it builds meet the same bar the rest of this spec sets (keyboard operability, assistive-technology exposure, plain language) — this spec does not re-litigate that surface's design, only names the dependency. It does, however, depend on that preference being readable consistently by every component that must honour it (the desktop daemon, the Shell extension, and the terminal client are separate processes) — the same cross-component configuration boundary already tracked as open in `docs/project-plan.md` T54, not a new one this feature introduces.
- Sound cues ship enabled by default with a per-cue toggle, matching the convention of comparable platform dictation features; the toggle is what this spec requires, the default is the assumption.
- The silence auto-stop interval defaults to a value tuned from real-world measurement rather than fixed here; this feature requires only that it exist, be configurable, and be perceivable before it fires. It shares its mechanism with the separately tracked silence auto-stop work.
- Sound cues and default shortcut choice/hardware-dictation-key support overlap with the separately tracked backlog items (state-transition chimes T61, trigger-alignment T58); this feature owns the accessibility requirements placed on them and delivers whatever portion is not already delivered, without redefining the underlying wire error model or picking the default shortcut combination.
- **Failure communication (US4, FR-023–FR-027) maps plain language onto today's ad-hoc error strings** (for example in `client/myna-desktop/src/controller.rs`), not a not-yet-built taxonomy. It overlaps with two separately tracked backlog items — the stable error-code taxonomy (`docs/project-plan.md` T31) and the error-state UX mapping (T62) — but is not blocked on either: this feature delivers plain-language, cross-surface failure communication against the current failure set, and adopts T31/T62's failures compatibly if and when they land, the same overlap treatment as T58/T59/T61 above.
- Backend provisioning concerns that are administrative rather than personal — installing models, choosing engines, privileged configuration — are out of scope except where they surface a user-facing message (FR-023) or a progress signal (FR-027).
- **Localisation of user-facing strings is out of scope.** No translation pipeline, translators, or locale roadmap exist for Myna today; requiring RTL layout support and pseudo-translation test infrastructure ahead of an actual translation effort would be speculative scope for a product this early. Existing i18n scaffolding in the Shell extension (gettext bindings, `N_()`-marked strings) is unaffected by this decision and continues to cost nothing to keep. If localisation becomes a real, scheduled effort, it is a separate feature.
- Out of scope, each for its own reason: a settings/preferences UI and first-run onboarding (a separate feature, as above); localisation (as above); text-injection accessibility including undo, edit-by-voice correction, and the behaviour of in-field unstable hypotheses under assistive technology (deferred to its own feature, with the risk recorded above); voice control of the desktop and wake-word activation (explicit product non-goals); accessibility of development-only tools; and desktops other than the primary target, which must not regress but are not gated here.
