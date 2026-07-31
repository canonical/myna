# Phase 0 Research: Switchable Basic Dictation HUD

**Feature**: 009-switchable-basic-hud | **Date**: 2026-07-31

## R1 - Persistent HUD preference

**Decision**: Add an extension-local GSettings enum schema with stable,
zero-based values and nicks `basic` and `wave`; default to `basic`. Declare the
schema in `metadata.json` and use `getSettings()` in both extension and
preferences processes.

**Rationale**: The choices are closed and stable, so a schema enum validates
stored values and makes the default explicit. GNOME Shell resolves an
extension-local `schemas/` directory, and settings propagate across the separate
preferences and Shell processes.

**Alternatives considered**:
- Unconstrained string key: easier to typo and admits invalid values.
- Command-line or developer-only switch: not a user preference and does not meet
  the feature.
- Reuse desktop-interface settings: wrong ownership and risks namespace clashes.

**Primary sources**:
- https://gjs.guide/extensions/development/preferences.html
- https://gjs.guide/extensions/overview/anatomy.html
- https://docs.gtk.org/gio/class.Settings.html

## R2 - Preferences process

**Decision**: Implement `prefs.js` with `ExtensionPreferences` and
`fillPreferencesWindow(window)`, presenting one `Adw.ComboRow` backed by a
`Gtk.StringList`. Map the zero-based enum value explicitly to `selected`.

**Rationale**: This is the standard GNOME 45+ preferences API and remains valid
for Shell 50/51. `prefs.js` runs in a separate GTK4/libadwaita process and must
not import `St`, `Clutter`, `Meta`, or Shell UI modules.

**Alternatives considered**:
- A panel-menu switch: adds persistent chrome and interaction scope not requested.
- A custom preferences window: duplicates the standard extension mechanism.
- Direct binding without mapping: obscures the enum-string versus unsigned-index
  types; explicit mapping is clearer for two values.

**Primary sources**:
- https://gjs.guide/extensions/development/preferences.html
- https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/class.ComboRow.html
- https://docs.gtk.org/gtk4/class.StringList.html

## R3 - Live preference observation

**Decision**: In `enable()`, obtain settings, connect
`changed::hud-style`, then read the key and create the selected view. In
`disable()`, disconnect the handler before dropping settings and controller.

**Rationale**: GSettings changes propagate between processes. The detailed
signal avoids unrelated callbacks, and explicit disconnection satisfies Shell
extension lifecycle rules. Reading after connection avoids a race and follows
GLib's changed-signal semantics.

**Alternatives considered**:
- Polling: unnecessary latency and a permanent timer.
- Apply only at next session or Shell restart: violates live switching.
- Have `prefs.js` call the extension directly: creates an unnecessary IPC seam.

**Primary source**: https://docs.gtk.org/gio/signal.Settings.changed.html

## R4 - Presentation-independent lifecycle ownership

**Decision**: Add an `IndicatorController` above both views. It owns current
wire descriptor, displayed/held descriptor, notice deadline/timer, dismissal
state, selected style, active view, and latest level with original arrival time.
Views become rendering-only.

**Rationale**: The current `HudView` owns `_held`, `_holdTimer`, and cached
untimestamped levels. Destroying it during a style change would cancel or restart
a notice, make stale audio fresh, and risk resurrecting an explicitly dismissed
critical error. Duplicating this logic in `BasicHudView` would create two policy
implementations guaranteed to drift.

**Alternatives considered**:
- Leave notice policy in each view and transfer state: exposes renderer internals
  and makes every future view responsible for semantic migration.
- Defer switching until idle: conflicts with immediate switching requirements.
- Re-query D-Bus after switching: D-Bus state cannot encode local dismissal or
  remaining auto-dismiss lifetime.

## R5 - Recoverable notice timing

**Decision**: Store an absolute monotonic deadline in the controller and keep one
controller timer. A style switch replays the held descriptor but is not a new
notice occurrence and never changes the deadline.

**Rationale**: Absolute deadlines preserve the exact remaining duration through
any number of switches and avoid transferring toolkit timeout handles.

**Alternatives considered**:
- Restart the full timeout in the new view: violates the edge case and lets users
  prolong notices by switching.
- Store remaining milliseconds and recreate on every switch: accumulates timing
  error and couples migration to timer mechanics.
- Let the old view finish timing while hidden: leaves a retired callback alive.

## R6 - Timestamped level replay

**Decision**: Every level forwarded to the controller carries a monotonic
`receivedAt`; `setLevel(rms, peak, receivedAt)` passes that timestamp to either
view. Switching replays the original timestamp.

**Rationale**: Level freshness is part of the UI contract. Replaying only numeric
RMS/peak through today's `setLevel()` stamps “now,” causing old energy to reappear
as fresh. Repeated equal-valued D-Bus updates still receive new timestamps.

**Alternatives considered**:
- Replay numeric values with current time: stale-audio resurrection.
- Reset to zero on switch: creates a visible discontinuity during recording.
- Put timestamp logic separately in each view: duplicates a contract boundary.

## R7 - Basic energy bar calibration

**Decision**: Reuse `vumeter.js`'s hardware-calibrated
`levelsToIntensity(rms, peak, ageMs)`, normalize its `FLOOR` to a zero-based fill,
then apply a pure fast-attack/slower-release smoothing step. Non-recording states
always target zero.

**Rationale**: Existing calibration already makes normal speech visible and
handles repeated/stale arrivals. The wave's nonzero “alive” floor is unsuitable
for a progress bar that must empty, so normalization preserves calibration while
meeting the basic HUD semantics.

**Alternatives considered**:
- Raw linear RMS: normal speech would barely move, a regression already found in
  feature 004.
- Reintroduce segmented/color-zoned meter logic: not requested; the reference is
  a simple continuous audio OSD bar.
- Reuse wave smoothing/phase model wholesale: couples a basic meter to decorative
  ribbon semantics.

## R8 - View structure

**Decision**: Keep `HudView` in `hud.js` as the wave renderer and add
`BasicHudView` in `basic.js`. Put normalization and injected-constructor
selection in Shell-independent `view-selection.js`; let `view.js` supply the
real actor constructors to `createView(style, {onDismiss})`. Do not rename
`HudView` or add an abstract base actor.

**Rationale**: The established view seam already supports independent renderers.
The extra pure selection module prevents headless tests from importing
Shell-dependent `St`/Clutter actors while still testing fallback and option
forwarding. One new view plus a shared controller remains smaller and clearer
than a widget framework. Avoiding a broad rename protects feature-004 history
and tests.

**Alternatives considered**:
- Rename all wave files/classes: cosmetic churn with no product value.
- One giant conditional view: entangles two visual implementations and makes
  “wave unchanged” difficult to verify.
- Abstract base class: two implementations do not justify inheritance.

## R9 - Schema packaging

**Decision**: Keep only the XML schema in source. Use `gnome-extensions pack`
for package validation; packaged installation compiles schemas automatically.
For the existing raw-copy developer path, run `glib-compile-schemas` in the
installed local `schemas/` directory. Never commit `gschemas.compiled`.

**Rationale**: GNOME's pack/install tools discover and compile local schemas;
manual copies do not. A package smoke check catches missing metadata/schema
integration before runtime.

**Alternatives considered**:
- Commit generated schema cache: architecture-specific generated artifact and
  easy to stale.
- Install globally under `/usr/share`: requires privilege and is wrong for the
  current user-local development install.
- Keep raw-copy instructions unchanged: `getSettings()` would fail at runtime.

**Primary sources**:
- https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/gnome-50/subprojects/extensions-tool/src/command-pack.c
- https://gitlab.gnome.org/GNOME/gnome-shell/-/blob/gnome-50/subprojects/extensions-tool/src/command-install.c

## R10 - Test and CI boundary

**Decision**: Test controller, settings normalization, and meter behavior
headlessly with injected views/settings/clock/scheduler; validate the package;
manually accept real actor geometry, focus behavior, high contrast, and visual
smoothness. Add Workshop `gjs-test` execution to CI.

**Rationale**: Pure behavior is deterministic without a display server, while
GNOME Shell's Clutter fork cannot safely construct extension actors outside a
running compositor. The repo already defines the Workshop action but CI does not
currently invoke it.

**Alternatives considered**:
- Actor unit tests under plain GJS: abort outside a running compositor.
- Manual-only testing: misses timer, switching, schema, and stale-level
  regressions.
- Add a new browser/npm harness: unnecessary dependency and unlike runtime.

## R11 - D-Bus and snap scope

**Decision**: Make no D-Bus publisher, Rust, or snap changes. The extension
already receives state, content-free reason, RMS, and peak. The client snap
publishes the interface but does not package the Shell extension.

**Rationale**: HUD style is a local presentation preference. Adding it to the
wire would violate ownership and create cross-process coupling.

**Alternatives considered**:
- Publish preferred style from `myna-desktop`: wrong user-settings owner and
  unnecessary protocol change.
- Package the extension in `myna-snap`: crosses the existing packaging boundary
  and is not required for the feature.
