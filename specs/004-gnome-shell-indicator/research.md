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
property + a `StateChanged(s state, s error_message)` signal; levels as
`AudioRms`/`AudioPeak` `d` properties updated at ~15–20 Hz; optional
`Start()`/`Stop()`/`Toggle()` methods.

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

**Decision**: A center-top **hanging blob** drawn in a `St.DrawingArea` via Cairo
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
asserting a `zbus` client observes `StateChanged` and the properties. (b)
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

## Open items carried to the plan / future

- Whether the constitution should explicitly name **GJS UI shims** as an
  evaluation-harness-tier carve-out (today argued by analogy to the Python
  testbed) — a possible constitution PATCH follow-up, not blocking.
- Public distribution channel (R12) — follow-up.
- A future non-GNOME focus-safe overlay (`gtk4-layer-shell` for wlroots/KDE)
  behind the same `org.myna.Dictation` contract — out of scope, contract-ready.
