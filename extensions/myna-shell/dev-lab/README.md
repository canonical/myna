# dev-lab — wave-ribbon tuning app

A small standalone GTK4 + libadwaita application for fast iteration on the
HUD's wave-ribbon animation (feature `004-gnome-shell-indicator`, the
2026-07-30 wave-ribbon redesign; research `R20`).

**Not part of the shipped extension.** It is excluded from `../metadata.json`
and the install step in `../../../specs/004-gnome-shell-indicator/quickstart.md`
— it exists purely to speed up development, and carries none of the shipped
extension's contract guarantees as its own.

## Why this exists

A GNOME Shell extension has no live-reload story: Wayland removed the nested
compositor (there's no `gnome-shell --nested`, no `mutter-devkit`), so the
only way to see a change to `hud.js` is a full session relogin. This app
sidesteps that entirely — edit, relaunch, see the change in under a second —
while still exercising the **real production code**, not a mock:

- `ribbon.js`, `ribbonPaint.js`, and `accent.js` are imported **unmodified**
  from the parent directory — the exact same modules `hud.js`'s
  `WaveRibbonActor` uses. Tune here, the shipped extension draws
  pixel-identical output; there is no separate "port to the extension" step.
- `../dbus.js`'s `DictationService` is imported **unmodified** too (it has
  zero Shell/`St`/`Clutter` dependency — pure `Gio`/`GLib` — so it runs
  unchanged outside the Shell process). With a real `myna-desktop --dbus`
  running, the ribbon reacts to **genuinely live** audio/state, not
  simulated data.

## Launch

```sh
cd extensions/myna-shell
gjs -m dev-lab/main.js
```

No build step, no install, no packaging — just `gjs` running the file
directly. Requires GTK4 + libadwaita GObject-introspection typelibs (present
on any current GNOME desktop; verified in this project's dev environment
against GJS 1.88, GTK4 4.23, libadwaita 1.10).

## What's in the window

- **Ribbon canvas** — the wave ribbon itself, painted via the shared
  `paintRibbon`, redrawn on a timer.
- **Manual level override** — a switch + slider to feed a fake RMS/peak
  level, for tuning without needing to actually speak into a live session.
  Off by default (the canvas follows the real live D-Bus level).
- **Lifecycle phase buttons** — `unfold` / `flow` / `relax` / `morph` /
  `complete` — jump straight to any phase on demand. A live session also
  drives these automatically (`transcribing` → `morph`, `finalizing` →
  `complete`, via the same `ribbonPhaseForStateKey` helper `hud.js` uses).
- **Severity buttons** — `recoverable` / `critical` / `clear` — simulate the
  HUD pill hiding the ribbon during a notice/error (X19-style), without
  reproducing the full pill UI.
- **Reduced motion** — forces the static/reduced-motion rendering path
  regardless of the real system setting, for previewing that fallback.
- **Tunable sliders** — strand count and points-per-strand, live. Other
  tunables (phase durations, envelope Hz, flow speed/frequency) are
  constants at the top of `../ribbon.js` — edit and relaunch to tune those;
  see "Iteration loop" below.
- **Dictation target** — a plain `Gtk.TextView`. It needs **no special
  handling** to be a valid IBus injection target: the injector
  (`client/myna-desktop/src/inject/ibus.rs`) has no app/toolkit
  special-casing at all and only refuses `GtkInputPurpose` PASSWORD/PIN — an
  ordinary multi-line free-form text view is exactly as valid a target as
  any other app's text field.

## The real end-to-end test loop

1. Start a real backend, e.g. `myna-server --adapter whisper --socket /tmp/myna.sock`.
2. Run `myna-desktop --dbus --socket /tmp/myna.sock --language en &`.
3. Launch this app; the status line should read `org.myna.Dictation:
   connected` shortly after.
4. Click into the text area (it grabs focus automatically on launch) and
   press the real configured hotkey to start dictating.
5. Speak — watch the ribbon react to your actual voice **and** the
   transcript land in the text area when the session ends.

Session start/stop stays hotkey-driven — `org.myna.Dictation`'s
`Start`/`Stop`/`Toggle` methods are still an unimplemented stub
(`DbusTrigger`, tracked separately under US4) — this tool doesn't add a
start/stop button.

## Iteration loop

```sh
# edit ribbon.js / ribbonPaint.js / accent.js, then:
Ctrl+C
gjs -m dev-lab/main.js
```

Sub-second relaunch, no extension install/reload/relogin needed. An
`entr`/`watchexec` one-liner works too if you want auto-relaunch on save,
e.g.:

```sh
ls ../ribbon.js ../ribbonPaint.js ../accent.js main.js | entr -r gjs -m main.js
```

## Scope

This tool has no independent functional requirements, no test-first
obligation, and no performance-watermark baseline (plan.md Constitution
Check) — narrower than even the shipped extension's harness-tier exemption,
since it isn't shipped at all. It is not part of the extension's own
manual-acceptance quickstart (`X1`-`X29`) and has no pass/fail gate of its
own.
