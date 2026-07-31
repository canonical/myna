# Phase 0 Research: GNOME Shell Extension for Myna Dictation UI

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

Resolves the Technical-Context unknowns and the spec's deferred design axes. Each
entry: Decision / Rationale / Alternatives considered.

## R1 — In-compositor overlay mechanism (the core problem)

**Decision**: A GNOME Shell extension (GJS), running inside Mutter, owning an
`St`/`Clutter` actor added to `Main.layoutManager` (a `Chrome` actor, not a
window). No window, no `wl_surface`, so no focus stealing by construction.

**Rationale**: `docs/desktop-injection.md` §2 (and feature 003 R6) establish that
on GNOME there is *no sanctioned way for a normal client to show an always-on-top,
non-focus-stealing overlay*. `wlr-layer-shell` is not implemented by Mutter; a
GTK toplevel can grab focus and cut dictation (the failure that sidelined
`indicator::gtk`). An extension is the GNOME-blessed path — Shell chrome actors
never participate in the keyboard-focus chain.

**Alternatives considered**: (a) `gtk4-layer-shell` overlay — wlroots/KDE only,
not GNOME; kept as a possible non-GNOME backend elsewhere. (b) `NotifyIndicator`
(toasts) — works today, is the fallback, but cannot host a persistent animated
goop / VU / model-loading glow. (c) A borderless override-redirect X11 window —
not applicable on Wayland and still focus-fragile.

## R2 — Extension ↔ myna-desktop transport

**Decision**: A **session-bus D-Bus interface** `org.myna.Dictation`, served by
`myna-desktop`, consumed by the extension via `Gio.DBusProxy`. State as a `State`
property (+ `ErrorMessage`), every update pushed with the standard
`PropertiesChanged` — the interface defines no custom signals, because
`PropertiesChanged` on the service's own path is the one broadcast a
strictly-confined publisher may send to unconfined subscribers (contract
§Confinement); levels as `AudioRms`/`AudioPeak` `d` properties updated at
~15–20 Hz; optional `Start()`/`Stop()`/`Toggle()` methods.

**Rationale**: D-Bus is the native GNOME IPC; `Gio.DBusProxy` gives the extension
property-caching + signal subscription for free. `myna-desktop` already vendors
`zbus` (feature 003 IBus engine) and already computes levels
(`myna_audio::AudioStats` behind a `watch::Receiver`) and owns the FSM state — so
the publisher is a thin adapter over existing seams, no new dependency. Matches
the landscape brief's `org.myna.Dictation` sketch.

**Alternatives considered**: (a) The existing Unix control socket
(`ControlTrigger`) extended to carry state/levels — bespoke framing, no
introspection, awkward from GJS (D-Bus is idiomatic there). (b) A file/FIFO the
extension polls — laggy, racy, no typed contract. (c) The WebSocket wire — that
is the client↔server transcript path, wrong layer, and would risk leaking
transcript content toward the UI. D-Bus keeps a clean state-only seam.

## R3 — State vocabulary and mapping

**Decision**: The interface `State` is a lowercase string enum:
`idle | loading | recording | transcribing | finalizing | error`. It is derived
in `myna-desktop` from the existing `IndicatorState` the controller already drives
(`Hidden→idle`, `Recording→recording` — but a cold-load window surfaces as
`loading`, see R4 — `Transcribing→transcribing`, `Finalizing→finalizing`,
`Error→error`). The extension maps `State` → a *visual intent* in the pure
`states.js`; an **unknown** value maps to a neutral "active" intent (never
throws), satisfying spec FR-008.

**Rationale**: Reuses the controller's audited `IndicatorState` (privacy-clean:
labels/states only, no transcript). Aligns with the project's internal liveness
phases (`transcription.progress` `preparing`/`ready`/`transcribing`). A string
enum is trivially forward-compatible (additive, matching the project's
unknown-is-ignored transport rule).

**Alternatives considered**: An integer enum (fragile across versions), or
exposing the raw `OrchestratorEvent` (leaks structure and risks transcript
fields). A string state property with an additive contract is simplest and safest.

## R4 — Distinguishing "model loading" from "listening" (FR-006)

**Decision**: Introduce a distinct `loading` interface state. Today the
controller maps both `Loading` and `Ready` orchestrator events to
`IndicatorState::Recording` (a cold load is "listening, warming up"). For the
richer goop we split: while the backend has emitted `Loading` but not yet `Ready`,
the `DbusIndicator` publishes `loading`; on `Ready`/first audio it publishes
`recording`. This is additive to the interface and internal to the publisher;
`NotifyIndicator` is unaffected (it keeps its coarser mapping).

**Rationale**: FR-006 requires the cold-model load be legible as *loading* and not
mistaken for *listening* (the "red glow" in the brief). The orchestrator already
emits `Loading` vs `Ready` distinctly; the publisher just stops collapsing them.

**Alternatives considered**: Reusing `recording` with a separate "warming" boolean
property — more surface, and a single string state is what the extension switches
on anyway. Changing `IndicatorState` itself — larger blast radius on feature 003;
the split is confined to the D-Bus publisher.

## R5 — Audio-level source and cadence

**Decision**: The publisher subscribes to `myna_audio`'s
`watch::Receiver<AudioStats>` (already exposed via `CaptureSource::stats()`), and
a small tokio task publishes `AudioRms`/`AudioPeak` (the normalized `[0,1]` `rms`/
`peak` fields) to the D-Bus properties at ~15–20 Hz, only while a session is
active. Between sessions (idle) it publishes zero. The extension applies its own
**stale-decay**: if no level update arrives within ~300 ms, the VU eases to floor
(spec FR-011 / SC-004) rather than freezing.

**Rationale**: Levels already exist, are privacy-clean (energy, not samples,
constitution V / audio-adapter §1.4), and update at capture time (the meter moves
even while the push is gated on model readiness). Throttling to ~20 Hz keeps D-Bus
traffic and repaint load modest while looking smooth.

**Alternatives considered**: Emitting a `LevelChanged` signal per chunk (~10 Hz
already, but a property with change-notification is cheaper for the proxy to
cache and lets the extension pull the latest without buffering). Pushing raw PCM
(privacy violation, wrong layer). Property + throttle is the balance.

## R6 — Goop geometry & animation (deferred design axis)

**SUPERSEDED (2026-07-22, on-hardware review with @cdunn)** — the center blob
read as tightly-packed, the colour set was unintuitive (purple-while-speaking),
and there was no visible status text or persistent error. Replaced by an
**experimental `RibbonView`**: a ~80%-monitor-width ribbon under the panel with
a Cairo **bar VU** driven by the live level, a content-free **status label**,
a conventional palette (amber loading / green listening / blue transcribing /
green finalizing / red error), and **errors held visible with their reason**
before clearing. Crucially, *all* presentation now sits behind a new
**`IndicatorView` seam** (`view.js`: `show`/`setLevel`/`hide`/`destroy` +
`createView()` factory), with `states.js` reduced to a semantic descriptor
(`{key, statusText, isError, hidden}`). The contract, proxy/lifecycle, state
set, and Rust level pump are the *certain* layer; the look is swappable
(team redesign / future user theme) without touching them. Both R6 and R7
below are the original blob-and-glow vision, kept for context; the ribbon is
likewise provisional. Diagrammed inline in `indicator.js`/`view.js`.

**SUPERSEDED AGAIN (2026-07-30, HUD redesign)** — the Ribbon's ~80%-monitor-width
top-of-panel presentation is itself replaced by a **bottom-center HUD pill**
styled after GNOME's own volume/brightness OSD (a much narrower, compact
element, not a wide ribbon). `RibbonView`/`indicator.js` is deleted outright
(not kept as a selectable alternate — see spec Assumptions); a new `hud.js`
implements the same `IndicatorView` seam. The descriptor shape in `states.js`
is reshaped from `{key, statusText, isError, hidden}` to `{key, statusText,
severity, hidden}` where `severity` is `'recoverable' | 'critical' | null`,
to carry the new two-tier problem distinction (R13). See R14/R15/R16 below for
the HUD's specific design decisions. The Rust contract, proxy/lifecycle
(`dbus.js`), and level pump are unaffected — this redesign only replaces the
*view* half of the seam plus reshapes the pure descriptor it consumes.

**Original decision**: A center-top **hanging blob** drawn in a `St.DrawingArea` via Cairo
(a rounded droplet whose bottom edge wobbles), sized ~à la an OSD, positioned by
`Main.layoutManager` just under the panel. Animations via
`Clutter.PropertyTransition` / `ease()`:
- **loading**: slow warm-amber pulse (opacity + scale breathing).
- **recording**: Gemini-style concentric ripple + a glow whose radius tracks
  `AudioRms` (the VU is the glow, R7).
- **transcribing**: a rotating/among-dots processing shimmer.
- **finalizing**: a single confirming flash, then fade.
- **error**: a brief red flash + a short horizontal shake, then clear.

Exact curves/colours live in `stylesheet.css` + `states.js` visual-intent
constants, tunable without touching actor wiring.

**Rationale**: Keeps the spec's "delightful, premium, legible" bar while confining
the tunables to CSS + one pure module. Clutter transitions are GPU-composited, so
≈60 fps without blocking the compositor (spec FR-009 / SC-007).

**Alternatives considered**: A pill/bar (less distinctive), a full waveform
(needs a sample stream we deliberately don't ship — privacy), or a panel-icon-only
treatment (fails the "goop" vision). The blob-with-glow-VU is the brief's intent.

## R7 — VU representation (deferred design axis)

**SUPERSEDED (2026-07-30, HUD redesign)** — replaced by R16's segmented bar
meter. Kept for context below.

**Decision**: **Glow intensity + radius** driven by `AudioRms`, with `AudioPeak`
gating an occasional brighter rim — i.e. the VU *is* the goop's aura, not a
separate bar. No numeric readout, no waveform.

**Rationale**: Ties level feedback to the single focal element (cohesive, premium),
carries only energy (privacy), and degrades gracefully via stale-decay (R5).

**Alternatives considered**: A discrete segmented bar or circular arc (more
chrome, less "alive"); a waveform (needs samples — out). Glow is the most
integrated with the goop.


## R8 — Panel presence & trigger (spec FR-013/014, US4/P3)

**Decision**: An optional subtle `PanelMenu.Button` (a small symbolic mic/goop
glyph) that (a) reflects availability (dimmed when `org.myna.Dictation` is absent)
and (b) on click calls `Toggle()`. It follows GNOME HIG for panel buttons. The
goop overlay itself is separate and only shows during a session. If the button is
judged too intrusive it can be disabled by default (a one-line switch); the MVP
(US1/US2) does not require it.

**Rationale**: Keeps the P3 trigger optional and non-intrusive, and gives a "myna
is available" affordance without a persistent overlay (push-to-talk, FR-002).

**Alternatives considered**: Always-on overlay presence (violates push-to-talk),
or no panel affordance at all (loses the availability signal + click-to-toggle).
Optional panel button is the compromise the spec's FR-013 "MAY" invites.

## R9 — Availability / lifecycle robustness (spec FR-018, US1-5, edge cases)

**Decision**: The extension uses `Gio.DBusProxy` with `G_NAME_OWNER` watching
(`Gio.bus_watch_name`): dormant (no overlay) when `org.myna.Dictation` has no
owner; activates on name-appeared; clears the goop to idle on name-vanished
(myna-desktop crash/exit). On `disable()` it disconnects the proxy, removes the
watch, destroys actors, and cancels all timers/transitions (spec FR-021 — no
leaks). Re-`enable()` re-establishes cleanly (Shell restart / relogin).

**Rationale**: These are the exact expected conditions (US1-5, edge cases); name
watching is the idiomatic GJS pattern and avoids surfacing errors for a
not-yet-running daemon.

**Alternatives considered**: Auto-starting `myna-desktop` via D-Bus activation —
out of scope (the daemon lifecycle is owned elsewhere) and would fight the
push-to-talk model. Polling for the name — wasteful vs the appeared/vanished
callbacks.

## R10 — Accessibility (spec FR-022, SC-009)

**Decision**: The goop actor carries an accessible role/label updated per state
via St's a11y (`St.Widget` accessibility: set `accessible_name` to the same
human state label the `NotifyIndicator` uses, e.g. "Dictation: listening"), so
Orca announces state changes. Colours come from theme-aware CSS classes with a
high-contrast variant; legibility never relies on colour alone (shape/animation
also differ per state).

**Rationale**: Satisfies UD129 accessibility (FR-022) and the "state, not content"
privacy rule — the label is a state, never transcript. Shape+animation redundancy
covers colour-blind/high-contrast users.

**Alternatives considered**: No a11y (fails SC-009). A separate hidden `St.Label`
for AT — St widgets already expose `accessible_name`, so the extra actor is
unnecessary.

## R11 — Testing strategy split

**Decision**: (a) **Rust publisher** — hermetic tests over a `Bus` trait boundary
(a fake in-memory implementation records emitted signals/property sets), asserting
the `IndicatorState`→`State`-string mapping (incl. the R4 loading split), property
snapshots, and `DbusTrigger` edge/dedup; plus one env-gated (`MYNA_DBUS_TESTS=1`)
suite standing the real object on a session bus (`dbus-run-session` in CI) and
asserting a `zbus` client observes `PropertiesChanged` and the properties. (b)
**GJS extension** — factor the state→visual-intent + stale-decay logic into pure
`states.js`/`vumeter.js` and unit-test them with a GJS test runner against a stub;
lifecycle (connect/disconnect/unknown-state) tested against a stub proxy; the
compositor/animation/focus-safety behaviour verified by a **manual on-hardware
acceptance** (quickstart).

**Rationale**: Puts test-first rigour where the shipped logic is (Rust), matches
constitution II (real behaviour in a gated suite runnable on VM + hardware), and
respects the GJS harness-tier exemption for compositor-only behaviour.

**Alternatives considered**: Driving a headless GNOME Shell in CI to test actors —
heavy, flaky, and still not a real focus-stealing check; deferred to the manual
acceptance. Skipping the fake-bus hermetic layer — would make the publisher's
mapping untested-first (violates I).

## R12 — Packaging / distribution (deferred design axis)

**Decision**: For this feature, ship the bundle in-tree under
`extensions/myna-shell/` with an `install`/`enable` step in the quickstart
(symlink/copy to `~/.local/share/gnome-shell/extensions/<uuid>/`, `gnome-extensions
enable`). Public distribution (extensions.gnome.org review, Ubuntu archive, or
bundling in a snap alongside `myna-desktop`) is noted as **follow-up**, not
delivered here.

**Rationale**: The feature's value is the working focus-safe UI; EGO review and
archive packaging are independent release concerns. In-tree + manual install
unblocks the on-hardware acceptance now.

**Alternatives considered**: EGO submission as part of this feature (adds an
external review dependency on the critical path); snap-bundling (couples to the
inference-snap work, out of scope). Both deferred.

## R13 — Recoverable/critical severity representation (2026-07-30 HUD redesign)

**Decision**: Extend the existing `IndicatorState::Error(String)` variant to
`IndicatorState::Error { message: String, recoverable: bool }` (a field
addition, not a new top-level variant). `DbusIndicator::map_state` publishes
**two distinct additive `State` wire values** from it — `error` when
`recoverable == false` (critical), and a new `notice` when `recoverable ==
true` (recoverable) — reusing the existing `ErrorMessage` property for both
(broadened meaning, not renamed, so no interface break). A session that
completes with an empty/blank transcript (`SessionOutcome::Completed{transcript}`
where `transcript.trim().is_empty()`) publishes `notice` with a fixed
content-free reason ("No speech detected") instead of `idle`; every other
completion still publishes `idle` exactly as before. Both the live per-event
path (`event_to_indicator`'s `Done(_)` arm) and the finalize-block safety net
(`Ok(SessionOutcome::Completed{transcript})` in `controller.rs`) compute this
through one shared helper, `completion_indicator_state(transcript: &str) ->
IndicatorState`, so the two call sites can never disagree or race — whichever
fires first "wins" and the second is a no-op under `DbusIndicator::publish`'s
existing per-wire-state dedup (same value in, no re-publish).

**Rationale**: The empty-transcript case is a *successful* completion, not a
failure — fabricating a transient `error` state for it would be semantically
wrong and would force GTK/Notify indicators (feature 003, out of scope here)
to special-case a non-error event or else show a spurious error toast. A field
on the existing `Error` variant keeps the ripple mechanical (6 files, one-line
destructure updates) and lets feature-003's indicators ignore the new field
entirely, provably unchanged. Realizing the split as two wire `State` values
(rather than a separate `ErrorSeverity` property) costs nothing extra and is
purely additive per the contract's existing compatibility rule (§Compatibility,
dbus-interface.md): an unpatched extension build that doesn't recognize
`notice` degrades to the existing neutral "active" treatment (FR-008), never a
crash or a stuck error.

**Alternatives considered**: (a) A new top-level `IndicatorState::Notice(String)`
variant — identical 6-file ripple, no smaller, and reads as an unrelated
concept rather than an error severity; (b) a side-channel bypassing the
`Indicator` trait object (`Any`-downcasting to a `DbusIndicator`-only method)
— non-idiomatic, breaks the shared trait-object seam every indicator backend
relies on; (c) a separate `ErrorSeverity` D-Bus property alongside a
synthesized `error` state for the empty-transcript case — semantically
conflates a success path with an error, rejected. This is an interim,
client-inferred classification (spec Assumptions) — the true wire-level error
disposition remains T31/T62's job; this feature does not build that taxonomy.

## R14 — Bottom-center HUD positioning

**Decision**: `Main.layoutManager.addChrome` (the generic chrome API, not
`addTopChrome` which is panel-anchored) with manual positioning at
`monitor.y + monitor.height - HEIGHT - MARGIN`, horizontally centered on the
primary monitor — the same general placement GNOME's own volume/brightness OSD
uses. Reuses the existing `monitors-changed` re-position pattern already in
`indicator.js`'s `_position()` (renamed/carried into `hud.js`), just anchored
to the bottom edge instead of just under the panel.

**Rationale**: Matches the spec's FR-004 requirement and the reference design;
`addChrome` (vs. `addTopChrome`) is the correct API for chrome that isn't
panel-relative. No dependency on Shell's internal `OsdWindow` positioning
logic (kept as a custom widget per the Assumptions — avoids relying on a
private Shell UI internal that isn't a stable extension API).

**Alternatives considered**: Reusing Shell's internal `OsdWindow` class
directly — rejected (private API, no stability guarantee across GNOME
versions); keeping the prior top-of-panel position — rejected per the spec's
explicit repositioning requirement (FR-004).

## R15 — Replace-in-place / restart-timer state machine (spec clarify pass, FR-007a/FR-007d)

**Decision**: The HUD view keeps a single "held notice" slot per severity tier
(reason string +, for `recoverable` only, a timer handle). A new arrival of
the *same* severity while one is already showing replaces the reason/icon in
place rather than stacking:
- **`recoverable`**: replaces the reason and **restarts** the auto-dismiss
  timer in full (fresh ~3.5 s window) — so a second "no speech detected"
  right after the first doesn't clear on the original's now-stale schedule.
- **`critical`**: replaces the reason but there is no timer to restart — it
  simply remains persistent until the user dismisses it; the dismiss
  requirement is never waived by a replacement.

**Rationale**: Directly encodes the two clarify-pass decisions (concurrent
critical errors → replace-in-place; concurrent recoverable notices →
replace-in-place + restart timer). Keeping one slot per severity (not a queue)
matches how the D-Bus interface already models state as a single current
value, not a queue, and avoids UI complexity (stacked notices) for what the
spec's Assumptions call an incidental, low-frequency case.

**Alternatives considered**: Queuing multiple notices — rejected (spec
clarify pass Q1/Q2 explicitly chose replace-in-place over queuing); ignoring a
second arrival while one is showing — rejected (would hide a second, possibly
different, "no speech" occurrence a user might want to know about promptly).

## R16 — Segmented bar meter (replaces R7's glow)

**SUPERSEDED (2026-07-30, wave-ribbon redesign)** — the segmented/discrete bar
meter is replaced by R17's flowing wave ribbon. Kept for context below.

**Decision**: A fixed set of discrete vertical bars (24) whose active count
tracks the same `AudioRms`/`AudioPeak` inputs the prior glow used, with the
same stale-decay-to-floor behavior (R5, spec FR-011). Segments illuminate
left-to-right conventional-VU-style (not a symmetric spindle profile) and are
colour-zoned green/yellow/red by position, matching a real hardware VU meter.

**Rationale**: Matches the reference design's segmented/dotted meter look and
the spec clarify pass's explicit choice of a discrete bar meter over a
continuous waveform or blob (spec Clarifications, 2026-07-30 session). No new
D-Bus fields are needed — `AudioRms`/`AudioPeak` already carry everything a
bar meter needs.

**Alternatives considered**: Keeping a continuous smoothed waveform restyled
to fit the narrower pill — rejected per the clarify-pass decision; a numeric
readout — rejected (less legible at a glance, not what the reference design
shows). A symmetric "spindle" bar-height profile (tallest in the centre,
tapering to the edges — the initial implementation, mirroring the old
ribbon's shape) was tried and removed: it doesn't read as a VU meter and
gave no way to express green/yellow/red zones by position.

## R16a — VU calibration and level-update forwarding (2026-07-30, manual-test follow-up)

Two real bugs surfaced only in a live GNOME session (not catchable by the
headless test suite, which necessarily mocks the D-Bus proxy and Shell
actors) — see `extensions/myna-shell/README.md` §Testing for why.

**Bug 1 — flat meter regardless of audio.** `dbus.js`'s `_setLevel` dropped a
level update when its RMS/peak were numerically identical to the previous
one. `HudView` uses *arrival time*, not value, to detect a stale stream
(R5) — so a steady voice signal (which legitimately repeats the same
quantized RMS/peak for consecutive ~50 ms pumps) stopped refreshing that
timestamp and decayed to the floor after `STALE_MS`. **Fix**: forward every
level update regardless of whether the values repeat; only the *state*
descriptor (`states.js`) is deduplicated, never the level.

**Bug 2 — meter needed shouting to move.** The original `boostLevel` used a
generic exponential gain (`1 - exp(-6·level)`) tuned by guesswork. A live
capture against a Plantronics/Poly Blackwire C5220 headset measured real
speech at RMS≈0.009–0.024 / peak≈0.025–0.067 (linear full-scale) against a
noise floor of ≈0.00003 — an exponential gain calibrated for "generic loud
audio" left normal speech barely above the floor. **Fix**: a calibrated dBFS
mapping (`DB_FLOOR = -67`, `DB_CEILING = -14`) derived from that measurement,
so normal conversational speech lands around the middle of the meter (not
its floor) without needing to raise your voice. RMS and a weighted peak
(`PEAK_WEIGHT = 0.55`) are combined so consonants/transients are visible
without a single spike pinning the meter. These constants are specific to
the measured hardware/gain chain and may need re-tuning for very different
microphones; there is no per-device auto-calibration (out of scope).

**Consolidation**: the pre-existing `levelToIntensity`/`levelToBars`
(single-value, symmetric-spindle-shaped) functions were removed once
`levelsToIntensity`/`intensityToActiveSegments`/`segmentColor` (RMS+peak,
left-to-right, colour-zoned) fully replaced their only caller (`hud.js`) —
dead code left over from R16's first pass would have been misleading
(its docstring still called it a "ribbon VU").

## R17 — Wave-ribbon meter (replaces R16's segmented bars)

**Decision**: Replace the 24-segment bar meter with a flowing "wave ribbon":
~3 translucent, layered strands, each 12–20 Cairo control points, all derived
from a **single** smoothed loudness envelope (the same `boostLevel`/stale-decay
math R16/R16a already established, reused unchanged) with small per-strand
phase/delay/amplitude offsets — never independent per-strand state, and never
raw audio samples or an FFT. Distinct behavior across the session lifecycle:
a ~150–200 ms unfold from the mic side on start, continuous left-to-right flow
while speaking (amplitude/brightness capped, FR-010c), a ~400–600 ms relax to a
thin idle line with an optional traveling pulse during pauses, and a smooth
morph into a simplified processing motion (a settling line or a few traveling
points) when recording ends into transcribing.

**Rationale**: Matches the external design decision doc's chosen "wave ribbon"
direction (distinctive/premium, avoids reading as an audio-engineering tool,
warmer/more organic than a conventional VU meter) while keeping the
performance envelope R16 already established: one shared envelope value, no
FFT, no particle systems, no per-frame texture regeneration — the compositor
interpolates motion at display refresh rate from a low-rate (20–30 Hz)
envelope update, exactly as the segmented meter did. Reuses R16a's calibration
work (dBFS mapping, RMS+weighted-peak blend, arrival-time-based stale-decay)
entirely — only the shape drawn from that same intensity value changes.

**Alternatives considered**: Keeping the segmented bar meter (rejected — this
redesign's whole premise, per the external design decision doc, is that a
discrete VU-style meter reads as a technical/audio-engineering tool rather
than a premium, native-feeling desktop indicator); a literal waveform of raw
audio samples (rejected on the same privacy grounds R7 already established —
the ribbon is synthesized from one energy envelope, never samples); an FFT-
driven "real" frequency visualization (rejected — needless CPU/GPU cost for a
value that is meant to represent voice presence/intensity, not spectral
content, and the project's audio-adapter layer doesn't expose frequency data
to the UI at all).

## R17a — "Fabric in gentle airflow," not an oscilloscope (2026-07-30 refinement)

**SUPERSEDED-IN-PART**: a follow-up design pass explicitly rejected the first
wave-ribbon pass's directness (raw envelope → wave amplitude, one frame at a
time) as reading too much like an oscilloscope — "nervous, noisy, overly
technical." This refines R17 rather than replacing it: the shared-envelope,
no-FFT, no-raw-samples constraints all stand; what changes is the shaping
between the envelope and the drawn wave, and the ribbon's behavior during a
recoverable notice.

**Decision**:
1. **A second, ~300 ms smoothing stage** (`ribbon.js`'s `applyEnvelopeSmoothing`,
   a one-pole low-pass with `SMOOTHING_TAU_MS = 320`, within the doc's
   250–400 ms range) sits between vumeter.js's calibrated instantaneous
   envelope (still updated ~20–30 Hz, still arrival-time stale-decaying) and
   the wave shape itself. This is deliberately a SECOND stage, not a
   replacement for R16a's calibration/stale-decay — the pipeline is
   `raw rms/peak → calibrated instantaneous envelope (vumeter.js) →
   smoothed envelope (ribbon.js, caller-maintained state across repaint
   frames) → wave shape`. The smoothing keeps syllables visible (a few
   hundred ms is fast enough to track real speech) while removing the
   frame-to-frame jaggedness of driving the shape directly off a 20–30 Hz
   instantaneous value.
2. **Four conceptual layers**, matching the doc's "layered construction":
   a `base` strand (slow, low-amplitude, nearly independent of the voice —
   keeps the ribbon "alive" even in silence), a `voice` strand (the main,
   most-reactive strand, with per-point "crest" brightness so louder
   syllables blend toward the warm highlight tone), and a `secondary`
   strand (delayed, less opaque, for depth). A 4th layer — sparse particle
   highlights on strong-syllable onsets — is scoped as **detection only**
   in this pass (`ribbon.js`'s `isStrongSyllableOnset`/
   `PARTICLE_ONSET_THRESHOLD`/`PARTICLE_LIFETIME_MS` are implemented and
   unit-tested) with actual particle rendering deliberately deferred: the
   design doc itself flags particles as optional and cautions that too
   many "would make it look like a music visualizer" — a real risk this
   feature keeps front-of-mind (R7's original glow decision was rejected
   for exactly that kind of overreach).
3. **`morph` now renders 3 travelling dots** as the wave crossfades out
   (not just an amplitude-reduced wave) — "contracts into a line or three
   softly travelling dots." **`complete` now converges toward a single
   centred point** with the existing brightness pulse, rather than just
   shrinking amplitude to near-zero.
4. **Retimed phase durations** to the doc's specific ranges: unfold 175 ms
   (150–200), relax 500 ms (400–600), **morph shortened to 225 ms**
   (200–250, was 400 — "a morph, not an abrupt replacement" reads better
   fast), complete 400 ms (300–500, unchanged).
5. **Recoverable severity keeps the ribbon visible** — tinted amber
   (reusing the pill's existing amber, `stylesheet.css`'s
   `rgb(245,166,35)`, rather than inventing a second one) and gently
   pulsing (audio-reactivity paused, not frozen dead) — instead of hidden.
   This is a genuine behavior change from R16/the first R17 pass (which
   hid the meter for any `severity !== null`, matching the segmented
   meter's reference design) and from spec.md's original FR-007/data-model
   E4 wording — confirmed explicitly with the product owner before
   implementing (2026-07-30). A **critical** error still hides/collapses
   the ribbon entirely; only recoverable changed. `hud-logic.js`'s new
   `ribbonVisibleForSeverity(severity)` (`false` only for `'critical'`)
   and `ribbon.js`'s `severityTint` parameter (`descriptor.severity` passed
   straight through — the values already match `null | 'recoverable' |
   'critical'`) implement this.

**Rationale**: The first wave-ribbon pass technically satisfied FR-010/
FR-010a (a flowing, envelope-driven shape with phase behavior) but visually
read as too literal a signal-processing display — exactly the "audio
engineering tool" feel the whole redesign exists to avoid (R17's own
rationale). "Audio drives the energy of the animation, while the product
controls its shape" is the operative principle: the smoothing stage and the
layered/rounded rendering are what let the shape stay expressive rather than
reactive-to-every-tick. The recoverable-visible-amber change makes a
passing hiccup read as "still listening, minor issue" rather than "gone
dark," consistent with US2a's original intent (a recoverable issue should
never feel like the system stopped).

**Alternatives considered**: Keeping the first pass's direct envelope→shape
mapping (rejected by design review as too oscilloscope-like); a full,
always-on particle system for every syllable (rejected — the doc's own
caution, and this project's established pattern of rejecting "visualizer"
aesthetics, e.g. R7's rejected glow); hiding the ribbon for both severities
(rejected for `recoverable` specifically, per the explicit product decision
this session — kept for `critical`, where a collapsed/hidden ribbon plus the
pill's existing red border/mic-slash icon is judged sufficient and matches
"the ribbon collapses" from the doc).

## R17b — Filled, glowing ribbon body (visual pass against a reference mockup)

**Decision**: `ribbon-paint.js`'s Cairo drawing was rewritten from thin
stroked lines to a filled, glowing "ribbon body" per strand, matching a
reference mockup directly: each strand is a single closed path (a Catmull-
Rom spline through the wave's top edge, mirrored on the bottom, tapered
thinner near both ends via a raised-cosine `edgeTaper`) filled with a
left-to-right Cairo `LinearGradient` that both shifts color (shadow → main
→ highlight → main → shadow) and fades alpha in/out at the ends in one
construct. The `voice` strand gets an additional cheap "glow" — 3
progressively wider, fainter stroke passes of the same spline behind the
crisp fill, since Cairo has no native blur and this reads as bloom at HUD
scale. The darker/complement tone (aubergine for the orange fallback, R18)
is blended 60% toward the main color and darkened before use, so it reads
as a warm shadow/depth undertone rather than a visibly different purple
hue — an early attempt using the raw complement colour looked like a purple
stripe, nothing like the reference's uniformly warm red/orange/black
palette.

**Rationale**: The first filled-body attempt (per-segment flat-shaded
quads) produced visible facet seams and read as flat/pastel rather than
glowing; a single continuous gradient-filled path is both smoother
(no per-segment lighting seams) and cheaper (one fill call per strand
instead of ~15). Verified by rendering to a headless Cairo `ImageSurface`
(needs no display server) at the actual HUD ribbon size (160×32) and the
larger dev-lab canvas (420×100), and inspecting the PNG output directly
against the reference — iterated on colour blending and layer thickness
until the two were a close match. This is the first time in this feature's
history visual output was verified by rendering and inspection rather than
by reasoning about the code alone; recorded here so a future contributor
knows that option exists (`Cairo.ImageSurface` + `writeToPNG` needs no
Shell/GTK runtime at all).

**Alternatives considered**: Per-segment flat-shaded fills with per-point
crest-colour blending (the first attempt — rejected: visible facets, no
smooth gradient, more expensive per frame); a true Gaussian blur for the
glow (Cairo has no built-in blur primitive; would need a separate blur
library or manual image-space convolution — rejected as unnecessary cost
for a HUD-scale element where the cheap multi-pass stroke glow is visually
sufficient); keeping the raw colour-wheel complement for the shadow tone
(rejected — read as an off-palette purple stripe, nothing like the
reference's monochromatic warm palette).

## R17c — "Trailing smoke" edges (further visual refinement)

**Decision**: Softened `ribbon-paint.js`'s ribbon body further, per direct
feedback that it should read "a bit like trailing smoke": (1) the body's
thickness now billows gently along its length via a slow, low-amplitude
`driftWave` (a deterministic sine of position + elapsed time), rather than
a uniform taper; (2) the top/bottom boundary is now traced with its own
soft, translucent, widening strokes (`paintFeatheredEdges`) in addition to
the gradient fill, so the edge itself looks diffuse rather than a crisp
cutoff; (3) two thin wisp tendrils curl away from the main (`voice`)
strand's centreline — glow-stroke-only, no solid fill, each with its own
higher-frequency drift and a gradient that fades to nothing at both ends —
evoking a curl of smoke peeling off the main flow rather than a second
parallel band. `computeRibbonModel`'s returned model now echoes `elapsedMs`
through (additive; doesn't change the `strands` shape the existing X24
tests check) purely so `ribbon-paint.js` can drive these rendering-only
time-based effects without needing new geometry fields.

**Rationale**: All of this lives in the paint layer only — `ribbon.js`'s
strand geometry, phase timing, and envelope smoothing are untouched, so
this is a pure rendering refinement, not a behavior change. Verified the
same way as R17b: rendered to a headless `Cairo.ImageSurface` at the real
160×32 HUD size and the 420×100 dev-lab size, at multiple points in time,
and inspected the PNG output directly.

**Alternatives considered**: Applying the wisp/billow effect to the
strand's actual geometry in `ribbon.js` (rejected — would entangle a
rendering embellishment with the tested, deterministic model contract for
no benefit, since the paint layer already has everything it needs via the
newly-echoed `elapsedMs`); a true particle-based smoke simulation
(rejected — far more expensive than a HUD warrants, and this project has
already rejected "visualizer"-style richness twice, R7 and R17a's particle
deferral).

## R17d — Reactivity ballistics + activity-scaled effects (further refinement)

**Decision**: Three changes, all in response to direct feedback ("a bit more
reactive to the audio," "slightly more pronounced curling," "flatter when
no audio is coming through"):
1. **Attack/release ballistics** replace the single symmetric smoothing
   time constant: `applyEnvelopeSmoothing` now uses a fast `ATTACK_TAU_MS`
   (90ms) while the target is rising and the slower `RELEASE_TAU_MS`
   (280ms, `SMOOTHING_TAU_MS`'s new role) while falling — the standard
   "fast attack, slow release" ballistics real audio meters use. An
   explicit `tauMs` argument still forces a single symmetric constant
   (back-compat for the handful of tests that only care about
   convergence, not attack/release asymmetry).
2. **Curling and body billow now scale with actual voice activity**
   (`ribbon-paint.js` derives `activity` directly from the voice strand's
   own amplitude — already ~0 at idle, ~1 at loud, since that's exactly
   what drives its geometry): louder audio gets a bigger, more pronounced
   wisp curl and body billow; near-silence gets almost none.
3. **The glow/feathered-edge/wisp embellishments fade in via a smoothstep
   ramp** (`activityRamp`, 0 at/below 0.08 activity, 1 at/above 0.3) rather
   than always rendering at full strength. This was necessary, not just
   nice-to-have: layering several near-flat strands' multi-pass fake-glow
   (each a handful of discrete stroke widths, not a true blur) produced
   visible horizontal banding once everything sat at nearly the same,
   nearly-static position — exactly the "no audio" case. A hard on/off
   threshold was tried first and rejected (see below) in favor of the
   smooth ramp, so the effects fading in/out doesn't visibly "pop" as a
   real voice crosses the threshold.

**Rationale**: (1) directly serves "more reactive" while keeping the
relax/decay side smooth (unchanged from R17a's intent — pauses still ease
rather than flicker). (2)/(3) together serve both "more pronounced
curling" and "flatter when no audio" from the same mechanism: the
depth-layer/glow/wisp effects that make the ribbon feel rich and alive are
exactly the ones that (a) should scale up with real energy and (b) caused
banding when forced onto a static, near-flat shape — so gating them on
activity solves both problems at once. Verified the same way as R17b/R17c:
rendered to a headless Cairo surface across the activity spectrum (near-
silent, transition, moderate, loud) and inspected the PNG output at each
point to confirm no banding artifact and a smooth progression.

**Alternatives considered**: A hard `activity > threshold` on/off for the
embellishments (tried first — works for static renders but would visibly
pop in real-time animation as a voice crosses the threshold; replaced with
the smoothstep ramp); fading the embellishments' alpha linearly with
activity instead of gating (rejected — even a small amount of the
multi-pass glow/feather at low-but-nonzero activity still showed faint
banding, since the artifact is geometric/positional, not just "too
visible," so a low alpha didn't fully hide it — the ramp needed a true
lower floor of zero, not just a small value).

## R17e — Bugfix: the convergence dot never faded (found via live testing)

**Bug**: the `complete` phase's convergence dot (FR-010d's brief quiet-
success indication) had its `alpha` hardcoded to `1`, completely
disconnected from `completeProgress`/`brightnessBoost` — which correctly
rise-then-fall (0→1→0). The dot appeared, then simply stayed at full
opacity indefinitely, since nothing else resets the phase away from
`'complete'` except the *next* session starting. Visible in practice as a
persistent bright circle sitting in the middle of the ribbon after
finalizing, until a new recording began — reported directly from live use
of `dev-lab` (which has no per-session actor teardown to coincidentally
mask it, unlike `hud.js`'s real pill, where the same bug existed but was
largely hidden by the whole pill fading out ~200ms after finalizing → idle
in the common case).

**Fix**: `convergence.alpha` now reuses the exact same `brightnessBoost`
value, so the dot fades out in lockstep with the brightness pulse and is
fully invisible (alpha 0) well before the phase would ever need to change
away from `'complete'` — the fix doesn't depend on anything external
resetting the phase promptly. Verified by rendering to a headless Cairo
surface at phase-elapsed times of half the pulse (dot clearly visible,
matching the reported screenshot), the end of the pulse (alpha ≈ 0), and
20× the pulse duration (still alpha ≈ 0, simulating the reported "stays
until a new recording starts" scenario). New regression coverage in
`ribbon.test.js` asserts `convergence.alpha` tracks `brightnessBoost`
exactly, so a future edit can't reintroduce a hardcoded/disconnected alpha
here without a test failing.

## R17f — Attack tuned further (still not reactive enough)

**Decision**: After live use of R17d's first attack/release pass
(`ATTACK_TAU_MS=90`), direct feedback was that it still felt laggy —
tightened `ATTACK_TAU_MS` from 90ms to **35ms**. `RELEASE_TAU_MS`/
`SMOOTHING_TAU_MS` (280ms) is unchanged; the complaint was specifically
about responding to *getting louder*, not about the relax/decay side,
which stays within the original 250-400ms design range.

**Rationale**: At the real 24Hz repaint cadence (`DEFAULT_ENVELOPE_HZ`), a
silence→loud step now reaches ~95% of target within 3 frames (~125ms),
versus roughly 3× that with the previous 90ms constant — verified by
simulating the step response frame-by-frame rather than reasoning about
the time constant in the abstract, since "how many frames until it looks
caught up" is what actually matters perceptually at this repaint rate.
35ms is fast enough to feel near-immediate without being literally
instantaneous (an unfiltered instant jump is exactly the "oscilloscope"
feel this whole redesign exists to avoid) — some single-pole smoothing
remains, just with a much shorter time constant than the release side.

**Alternatives considered**: Removing attack-side smoothing entirely
(rejected — reintroduces the "nervous, tick-by-tick" feel R17a explicitly
rejected, just on the rising side only); raising `DEFAULT_ENVELOPE_HZ`
instead of shortening the tau (rejected — the repaint rate is already
within the 20-30Hz design range and doubling it would cost more CPU for a
worse fix than simply shortening the filter's time constant, which is free).
Test thresholds in `ribbon.test.js` calibrated to the old 90ms constant
were updated to match the new value rather than kept as a stale
regression check against a since-changed design decision.

## R18 — Accent-color source

**Decision**: `Gio.Settings` against the `org.gnome.desktop.interface` schema's
`accent-color` key (a 9-value enum: blue/teal/green/yellow/orange/red/pink/
purple/slate; GSetting added GNOME 47). Read via
`Gio.Settings.get_user_value('accent-color')`, not `get_string`/`St.Settings`:
`get_user_value` returns `null` only when the key was never actually written
by the user (including sitting on the untouched factory default, which is
itself `'blue'`) — this is the only mechanism that lets "the user genuinely
chose blue" and "the user never touched this setting" be told apart, both of
which read identically via `get_string`/`St.Settings.get().accent_color`. A
`null` result (nothing ever set) maps to a fixed Ubuntu-orange default
(`#E95420`); a non-null result resolves through a fixed 9-entry hex table
(from libadwaita's `Adw.AccentColor`: blue `#3584e4`, teal `#2190a4`, green
`#3a944a`, yellow `#c88800`, orange `#ed5b00`, red `#e62d42`, pink `#d56199`,
purple `#9141ac`, slate `#6f8396`) into a small derived palette (highlight,
darker/complementary secondary, translucent tone) for the ribbon's layered
strands. The darker/complementary tone is a computed colour complement of the
resolved main colour, **except for orange, whose darker tone is a fixed
aubergine** rather than a generic computed complement — matching the
reference design decision's explicit "aubergine if the main colour is
orange" rule (2026-07-30 analysis pass: this specific override had been
dropped to a generic "darker/complementary" description across the derived
artifacts; reinstated here and in spec.md FR-010b, data-model.md E2a,
contracts/extension.md X25, and tasks.md T054). Guarded by
`Gio.SettingsSchemaSource.get_default().lookup(...)` +
`GSettingsSchema.has_key(...)` before ever constructing a `Gio.Settings`
against the schema, so a pre-GNOME-47 shell (schema or key absent) degrades
safely to the same Ubuntu-orange default rather than crashing.

**Rationale**: Directly resolves the design decision doc's requirement ("use
the selected accent colour... or the default Ubuntu orange if the accent
colour is not set") together with the explicit product decision that an
*untouched* default must be treated the same as "not set," even though its
resolved value (`'blue'`) is indistinguishable from a deliberate choice of
blue without `get_user_value`. Read live via `changed::accent-color` on the
same `Gio.Settings` object so the ribbon re-colors if the user changes their
accent color while a session is active — no restart required.

**Alternatives considered**: `St.Settings.get().accent_color` (GNOME Shell's
own convenience singleton, also backed by this GSetting since GNOME 47) —
rejected as the *sole* mechanism because it only exposes the resolved enum
value, with no way to distinguish a genuine user choice of blue from the
untouched default; still usable as a secondary read for other purposes but
not for this fallback rule. Hardcoding Ubuntu orange unconditionally (no
accent-color theming at all) — rejected; the design doc explicitly calls for
accent-color theming when the user has one. Deriving lighter/darker tones via
libadwaita's `adw_accent_color_to_standalone_rgba` — unavailable to a GNOME
Shell extension (that API exists only for GTK4/libadwaita *applications*, not
in-compositor extensions), so the palette derivation is hand-rolled instead
(shared, tested pure logic — see R20's `dev-lab`, which as a libadwaita app
*does* have access to that API for its own chrome, but not for the ribbon
paint itself, kept identical to the extension's).

## R19 — Reduced-motion source

**Decision**: `org.gnome.desktop.interface`'s `enable-animations` boolean
(existing GNOME-wide setting, not new). When `false`, the ribbon renders a
static level line or a gently-scaling microphone indicator — still driven by
the same level/state inputs — instead of the flowing animation.

**Rationale**: Reuses an existing, well-established GNOME accessibility
setting rather than introducing a new one; consistent with spec FR-022a and
the constitution's general legibility/accessibility posture. Read the same
way as R18 (schema/key-guarded `Gio.Settings`, live-updated via `changed::`),
so both preferences share one small settings-reading pattern.

**Alternatives considered**: `org.gnome.desktop.a11y`'s toggles — a
narrower, accessibility-specific surface; `enable-animations` is the more
commonly-set, general-purpose signal and is what GNOME Shell's own chrome
already honors for its animations, so following it keeps the ribbon
consistent with the rest of the desktop's motion behavior.

## R20 — Standalone developer tuning tool (`dev-lab`)

**Decision**: A small, non-shipped GTK4 + libadwaita (`Adw.Application`) GJS
application under `extensions/myna-shell/dev-lab/`, launched directly
(`gjs -m dev-lab/main.js`, no install/build step) for fast iteration on the
ribbon's look and feel. It reuses `dbus.js`'s `DictationService` **verbatim**
(confirmed zero `St`/`Clutter`/Shell dependency — pure `Gio`/`GLib`) for a
genuinely live `org.myna.Dictation` connection, and the same shared
`accent.js`/`ribbon.js`/`ribbon-paint.js` pure modules the shipped `hud.js`
uses, so there is no separate "port to the extension" step — both consume
identical code from day one. Adds: a `Gtk.DrawingArea` painted via the shared
`paintRibbon`; manual-override controls (fake level slider, per-phase trigger
buttons, a reduced-motion toggle, live tunable sliders) so every animation
branch is reachable without a live mic session; and a plain `Gtk.TextView`/
`Gtk.ScrolledWindow` dictation target (confirmed a default free-form,
non-secure `GtkTextView` needs no special handling — the injector
(`client/myna-desktop/src/inject/ibus.rs`) has no app/toolkit special-casing
and only refuses `GtkInputPurpose` PASSWORD/PIN) so a real end-to-end session,
through IBus injection, can be exercised in the same window. Session
start/stop stays hotkey-driven (`DbusTrigger`/`org.myna.Dictation`
`Start`/`Stop`/`Toggle` remain unimplemented stubs, US4 — out of scope here).
`Adw.StyleManager`'s own `accent_color_rgba`/`system_supports_accent_colors`
is used only to tint the app's own libadwaita chrome (header bar, buttons) as
a bonus/debug aid — never to drive the ribbon paint itself, which always
resolves through the shared `accent.js` to guarantee parity with `hud.js`.

**Rationale**: GNOME Shell extension development has no viable live-reload —
Wayland has no nested compositor/devkit viewer (quickstart.md's existing
dev-loop note), so the only way to see a Shell-side change is a full session
relogin. A standalone app sidesteps this entirely (sub-second
edit→relaunch), while still exercising the *real* production code paths
(`dbus.js` unmodified, the same paint function) rather than a divergent mock,
and the text area closes the loop to a genuinely real, human-verifiable
end-to-end dictation test without needing a separate target application.

**Alternatives considered**: Iterating directly against the installed
extension (disable/enable or full session relogin per change) — far slower,
and this project's own quickstart already documents the Wayland nested-shell
gap that makes this painful. A pure-Cairo/no-toolkit harness (bare surface,
manual event loop) — more code for less: GTK4 already provides the window,
event loop, and widgets (slider, text view, header bar) needed, and its
`Gtk.DrawingArea` draw-func hands back an ordinary Cairo context, so the
shared `paintRibbon` function needs no adaptation either way. Building the
currently-stubbed `DbusTrigger` so the lab could start/stop sessions itself
— a materially larger, separate, TDD-bound piece of Rust scope (US4);
deferred, hotkey-driven start/stop is sufficient for this tool's purpose.

## Open items carried to the plan / future

- Whether the constitution should explicitly name **GJS UI shims** as an
  evaluation-harness-tier carve-out (today argued by analogy to the Python
  testbed) — a possible constitution PATCH follow-up, not blocking.
- Public distribution channel (R12) — follow-up.
- A future non-GNOME focus-safe overlay (`gtk4-layer-shell` for wlroots/KDE)
  behind the same `org.myna.Dictation` contract — out of scope, contract-ready.
- A true wire-level error disposition/taxonomy (T31/T62) that would let R13's
  interim, client-inferred severity classification be replaced by a real
  disposition carried end-to-end from the inference backend — this feature's
  classification is a stopgap, not that taxonomy.


