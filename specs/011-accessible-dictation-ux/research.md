# Phase 0 Research: Accessible Dictation UX

## R1 — Announcement mechanism (how a content-free state change reaches speech/braille)

**Decision**: Emit the AT-SPI2 protocol's `Announcement` event (`org.a11y.atspi.Event.Object`
interface, `Announcement` member — carrying `{text, politeness}`) directly on the
accessibility bus (`org.a11y.Bus`) from every surface, rather than going through a
per-toolkit convenience API alone:

- **`myna-desktop`** (shipped, headless — `DbusIndicator`/`NotifyIndicator` path,
  no GTK widget exists here): use the `atspi` crate (pure Rust, zbus-based,
  `docs.rs/atspi`) to connect to `org.a11y.Bus`, register one small always-present
  accessible object representing the dictation session (exposing accessible
  name/description/state so FR-001's on-demand query works even between
  transitions), and emit `Announcement` on each transition.
- **`GtkIndicator`** (opt-in `ui-gtk` feature): call `gtk_accessible_announce()`
  directly (stable since GTK 4.14) — GTK already emits the same underlying
  AT-SPI `Announcement` event internally, so no extra dependency is needed for
  this path beyond bumping the declared GTK feature level.
- **`extensions/myna-shell`** (GJS): St/Clutter has no `announce()`-equivalent
  (confirmed against the GNOME accessibility guide, which documents only roles,
  relationships, and states — no live/proactive announcement primitive). Emit
  the same `Announcement` event directly over `Gio.DBusConnection` to
  `org.a11y.Bus` (obtained via `org.a11y.Bus.GetAddress()`, the standard
  bootstrap), mirroring what GTK does in C.

*Two of those three surfaces were withdrawn during implementation, leaving
`myna-desktop` as the only announcing path.* `GtkIndicator` went with the
`ui-gtk` overlay, removed wholesale (project-plan T150), so no shipped build
has a GTK widget to announce from. The `extensions/myna-shell` announcer was
written and then deleted: the extension has no shipping vehicle, so an
announcement that fires only when a separately-installed extension is present
cannot carry a MUST, and two emitters gave the verbosity preference two places
to reach. The wire-level decision is unchanged and FR-002 still holds
literally — one `Announcement` event drives both speech and braille. What
changed is that "from every surface" is now satisfied by there being a single
surface, which also discharges FR-006 and FR-024a by construction rather than
by keeping two implementations in step.

**Rationale**: Using the same wire-level primitive from both languages is what
actually satisfies FR-006 ("identically") and FR-024a ("identical wording") at
the protocol level rather than by convention alone, and it satisfies FR-002's
"reaching both speech and braille through the same path" literally — braille
output is driven by the same AT-SPI event a screen reader consumes, not a
separate channel. It also gives FR-001's on-demand query for free: an
AT-SPI-registered accessible object's name/description/state are queryable by
any AT at any time, not only at the instant of a transition.

**Alternatives considered**:
- Shelling out to `speech-dispatcher`/`spd-say` directly — bypasses whatever AT
  the user is actually running (Orca vs. another AT, braille-only setups),
  duplicates routing AT-SPI already provides, and gives no programmatic-query
  surface for FR-001.
- Relying only on `gtk_accessible_announce()` via a GTK widget for every
  surface — would force the default (non-`ui-gtk`) `myna-desktop` build to
  depend on GTK, which the project deliberately removed from `default` features
  (project-plan T66, size-pruning).
- Mutating `Atk.StateType`/label text on an `St.Widget` in the Shell extension —
  does not reliably produce a *proactive* announcement decoupled from focus;
  it's the same limitation feature 004 already accepted for the visual layer.

## R2 — Sound-cue playback backend (FR-010/011)

**Decision**: Add a short playback stream on the existing vendored `pipewire`
crate (already a `myna-audio` dependency for capture), driven from
`myna-desktop`'s new `sound` module. Cues are short, pre-rendered PCM clips
embedded in the binary (no on-disk sound-theme lookup).

**Rationale**: No audio-output plumbing exists anywhere in the client today
(project-plan T61). `pipewire` is already vendored, already the constitution's
named primary audio server, and keeps cue playback and the capture graph on
the same audio server without adding a new system dependency. The alternative,
`libcanberra` (the freedesktop standard for UI sound cues, XDG sound-theme
aware), was seriously considered — it is genuinely the more idiomatic desktop
choice — but rejected because it pulls in a new C dependency plus XDG
sound-theme data files, working directly against this project's documented,
deliberate snap-size pruning (project-plan T66 removed GTK's icon themes and
related data for the same class of reason). This is flagged as revisitable: if
`libcanberra`'s footprint turns out to be small in the confined build, it can
be reconsidered without changing any requirement.

**Alternatives considered**: `libcanberra` (rejected above); `rodio`/generic
cross-platform audio crates (rejected — would open a second audio backend
alongside PipeWire, duplicating device/session management the project already
solved once in `myna-audio`).

## R3 — Coverage-matrix, contrast, and colour-only-encoding regression gates (FR-032)

**Decision**: One checked-in data file (`extensions/myna-shell/coverage-matrix.json`)
is the single source of truth for the state→channel mapping (spec Key
Entities). It is read by both a hermetic Rust test (`coverage.rs`, via a
`serde`-deserialized copy checked into the same path relative to the workspace
root) and a hermetic GJS test (`coverage.test.js`), each asserting the
invariant: every state has ≥1 visual and ≥1 non-visual channel, and no
state/severity is marked colour-only or sound-only. Contrast thresholds
(4.5:1 text, 3:1 non-text — FR-013) are checked against the shipped stylesheet
colour values with a small hermetic contrast-ratio calculator (WCAG
relative-luminance formula), not a live rendering/screenshot pipeline. *Planned
against `extensions/myna-shell/stylesheet.css`; as implemented the shipped
stylesheet is `client/myna-hud/src/style.css` and the check lives in
`myna_hud::contrast`, which `include_str!`s it so the gate cannot drift from
the file it describes (tasks.md T084).*

**Rationale**: A single shared data file makes divergence between the two
languages structurally impossible rather than merely reviewed-for; the
contrast check operates on the authored colour values already checked into the
stylesheet, so it runs hermetically in CI without a display (consistent with
FR-030's headless requirement) and catches regressions the moment a colour
value changes, without needing a live rendering/screenshot pipeline.

**Alternatives considered**: A screenshot-diffing/visual-regression pipeline —
rejected as heavier, non-hermetic (needs a real compositor, which the spec's
Edge Cases already flag as unavailable headlessly on Wayland), and unnecessary
since the actual authored colour values are already known statically.

## R4 — Reduced-motion / high-contrast / text-scale / forced-colours (FR-012, FR-013)

**Decision**: Read `org.gnome.desktop.interface` (`text-scaling-factor`,
`gtk-theme`/high-contrast variant) and `org.gnome.desktop.a11y.interface`
(`org.gnome.desktop.interface`'s reduced-motion is exposed through
`org.gnome.desktop.a11y.interface.enable-animations` invert) via GSettings,
same mechanism `accent.js` already uses for accent colour (feature 004) — no
new dependency. `GtkIndicator` and `myna-desktop`'s notification path defer to
the platform's own high-contrast/text-scale rendering (GTK/notification
daemon already scale correctly); this feature's obligation is to not hard-code
colours/sizes that would fight that scaling, verified by the contrast checker
(R3) and a manual acceptance step (quickstart.md) at 200% scale.

**Rationale**: Reuses an existing, already-proven read path (feature 004) with
zero new dependencies; keeps the platform, not this feature, responsible for
the mechanics of scaling text and swapping high-contrast palettes.

## R5 — Failure-taxonomy mapping (FR-023–027)

**Decision**: Per spec Clarifications/Assumptions (already resolved, not
re-litigated here): map plain language onto today's ad-hoc error strings in
`client/myna-desktop/src/controller.rs` and `inject/mod.rs`'s `InjectError`,
via one new `failure.rs` module containing a single `FailurePresentation`
struct (message, recovery action, severity) authored once per known failure
and rendered identically by the indicator, notification, terminal, and
announcement paths. `myna-cli` reuses the same struct for its stderr output
(US5/FR-029).

**Rationale**: Directly implements the spec's already-decided ownership split
(this feature owns cross-surface wording, not the wire taxonomy) with the
smallest structural change — one authoritative struct per failure, consumed by
every surface, so FR-024a's "identical wording" is structural rather than
reviewed-for.

## R6 — Headless AT-SPI verification limits for the Shell HUD (spec Edge Case, FR-030)

**Decision**: Document, rather than attempt to close, the gap: GNOME Shell's
nested-compositor mode is unavailable on Wayland, so the Shell extension's live
`org.a11y.Bus` emission and the resulting AT-SPI tree cannot be asserted
end-to-end without a real compositor session. The automated GJS suite instead
verifies the *pure* mapping (state → announcement text/politeness, coverage
matrix membership) that would be sent; the manual acceptance protocol
(quickstart.md) is the only place the real bus emission and the actual Orca
experience are verified end-to-end for the Shell HUD. `myna-desktop`'s own
`atspi`-backed announcer, by contrast, *is* fully testable headlessly (no
compositor needed — a session/`dbus-run-session` accessibility bus suffices),
via the `MYNA_ATSPI_TESTS`-gated suite.

**Rationale**: This matches the spec's own instruction (FR-030) to record the
gap rather than assume it closed, and mirrors feature 004's identical
treatment of GNOME Shell's headless-testing ceiling.

## R7 — Preference storage (project-plan T54) — resolved during implementation

**Decision**: This feature's plan originally treated preference storage as an
open dependency of the separate, out-of-scope settings-UI feature (spec
Assumptions), consistent with project-plan T54 being unresolved at planning
time. Between planning and implementation, `integration-220627` gained
`feat(client): Move settings into GSettings, with a snap-config default`: a
real `com.canonical.Myna.Dictation` GSettings schema
(`client/data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml`) and Rust
wrapper (`myna_core::settings::Store`/`Settings`), already used for
`streaming-mode`/`language`/`hud-style`/`silence-timeout`. This feature rebased onto
that work and extended the same schema with `announcement-verbosity` and
`sound-cues-enabled` rather than treating the
store as still-hypothetical. A third planned key, `silence-auto-stop-seconds`,
was dropped on a later rebase in favour of upstream's equivalent
`silence-timeout` (default `30`), which `myna_desktop::AutoStop` already
consumes end to end. `client/myna-desktop/src/preferences.rs`'s
`GSettingsPreferences` reads it via `Settings::load()`; the missing-schema
fallback (`Settings::default()`) was fixed during this work to return the
FR-004-mandated defaults for every field (a real bug caught by test: a naive
`#[derive(Default)]` gives `bool`/`u32` their primitive zero values, not the
schema's actual defaults).

**Rationale**: Building on the real, already-shipped store is strictly better
than the originally-planned wait-and-assume posture: it gives FR-004/FR-010/
FR-018's cross-process consistency requirement a concrete, already-tested
mechanism instead of a named-but-unsolved dependency, at no cost to this
feature's scope (the settings *UI* remains genuinely out of scope — this
feature only adds schema keys and a reader, never a control surface).

**Alternatives considered**: Keeping `preferences.rs`'s original
`DefaultPreferences`-only design and waiting for the settings feature to
define storage — rejected once the storage question was no longer open;
doing so would have meant deliberately ignoring already-landed, directly
applicable work. `DefaultPreferences` is kept (not removed) as the
dependency-free implementor hermetic seam tests use directly, so hermetic
tests never need a real or fake GSettings backend.
