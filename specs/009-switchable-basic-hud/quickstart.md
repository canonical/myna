# Quickstart / Validation: Switchable Basic Dictation HUD

**Feature**: 009-switchable-basic-hud | **Date**: 2026-07-31

Runnable validation for the contracts in `contracts/settings.md` and
`contracts/presentation.md`. Feature 004's D-Bus publisher and spoken-injection
acceptance remain prerequisites and are not duplicated here.

## Prerequisites

- GNOME Shell 50 or 51 on Ubuntu Desktop/Wayland.
- The Workshop environment from `.workshop/myna.yaml` or equivalent packages:
  GJS, GNOME Shell extension tools, GTK4/libadwaita, and GLib schema tools.
- `myna-desktop --dbus` and an inference backend for spoken acceptance.
- IBus and a focusable text field for focus/injection checks.

Before manual acceptance, record the named reference environment here: machine,
CPU/GPU, display refresh rate, Ubuntu/GNOME versions, Wayland session, and the
measurement tools/versions used. Use the same named environment for every
reported switch-latency, decay, and frame-rate result.

## 1. Headless GJS contracts

```sh
workshop run myna gjs-test
```

**Expected**: all existing feature-004 tests plus the feature-009 controller,
basic-meter, settings, and lifecycle tests pass. In particular:

- default/unknown style resolves to basic;
- pure view selection chooses injected basic/wave constructors and forwards the
  dismiss callback without importing Shell actors;
- hidden switching never shows a HUD;
- visible switching destroys then replaces and replays current state;
- level replay retains original timestamp;
- recoverable deadline is not restarted by switching;
- critical errors survive switching and dismissed errors do not resurrect;
- malformed/out-of-range meter input remains bounded and monotonic;
- stale/non-recording meter state reaches zero;
- 100 rapid switches leave one responsive view.

**Automated result (2026-07-31)**: PASS on the host GJS runtime; all existing
feature-004 and new feature-009 suites pass. The canonical Workshop action is
pending because `workshop` is not installed on this host; CI now invokes it.

## 2. Schema and package smoke

From `extensions/myna-shell/`:

```sh
workshop run myna gjs-package
```

**Expected**: schema validation and package creation succeed; the package
contains every imported runtime module, `prefs.js`, and the XML schema, but does
not contain `dev-lab/` or a source-controlled `gschemas.compiled`. The canonical
Workshop action owns the explicit `--extra-source` list; do not maintain a second
pack command here.

**Automated result (2026-07-31)**: PASS. Strict schema validation succeeds; the
ZIP contains every imported runtime module, `prefs.js`, and the XML schema, and
excludes `dev-lab/` and `gschemas.compiled`. The equivalent canonical Workshop
action is pending because `workshop` is not installed on this host.

## 3. Development install

Follow the single maintained development-install and error-diagnosis procedure
in [`extensions/myna-shell/README.md`](../../extensions/myna-shell/README.md#install-development).
Do not copy the older feature-004 install commands: they predate the required
local GSettings schema.

**Expected**: preferences opens with one HUD-style selector, Basic selected on a
fresh install. Selecting Wave ribbon persists immediately. Reopen preferences
and re-enable the extension to verify the chosen value remains.

For an isolated default test without changing normal user settings:

```sh
dbus-run-session -- sh -c 'gsettings --schemadir extensions/myna-shell/schemas get org.gnome.shell.extensions.myna hud-style'
```

**Expected**: `'basic'`.

## 4. Live switching during recording

Start the established feature-004 stack, focus a text field, and begin
dictation. While speaking, change HUD style in extension preferences.

**Expected**:

- replacement appears within 250 ms without Shell/session restart;
- old HUD disappears before the replacement is visible; never two HUDs;
- new HUD shows the current listening state and current energy;
- no interruption to capture, transcription, or final text injection;
- focus remains in the text field;
- both styles remain bottom-center on the primary monitor.

Measure switch latency with a 60 fps-or-faster screen recording: count frames
from the selector's visible value change to the first frame containing only the
replacement HUD. At 60 fps, 15 frames is 250 ms. Record the frame count, capture
rate, and result for both directions on the named reference environment.

Measure compositor smoothness during sustained recording with Sysprof's GNOME
Shell/Mutter frame-clock trace (or the platform's equivalent compositor frame
profiler). Record the exact profiler and observed frame cadence; the target is
approximately the display's 60 fps cadence on the named 60 Hz reference setup,
with no visible sustained stutter introduced by either HUD.

For the basic HUD, normal speech visibly fills the horizontal bar, louder speech
never lowers it, and silence/stale input empties it within 600 ms. Loading,
finishing, recoverable notice, and critical error retain their labels while the
bar decays to empty. A distinct Transcribing label is supported when explicitly
published, but is not part of the ordinary trigger-driven Listening → Finishing
path.

## 5. Recoverable notice switch

Start and stop without speaking to produce “No speech detected.” Wait about half
of the existing hold window, then switch style.

**Expected**: the notice changes presentation but keeps only its remaining hold
time. It does not receive a fresh full timeout. A subsequent genuine no-speech
occurrence receives the normal full hold window.

## 6. Critical error switch and dismissal

Produce a hard error (for example, unavailable microphone/backend), switch
styles, then click dismiss. Switch styles again before the source state changes.

**Expected**: the error survives the first switch, remains persistent and
dismissible, clicking dismiss does not steal keyboard focus, and the dismissed
error does not reappear after the second switch.

## 7. Robustness and cleanup

While active, alternate the preference 100 times using the preferences UI or
the schema key, ending on a known style. Then change state and level, move/change
monitor layout, disable the extension, and wait beyond the recoverable timeout.

**Expected**:

- exactly one final HUD responds;
- no retired HUD moves, repaints, dismisses, or reappears;
- disabling removes actors and all callbacks;
- changing the preference after disable has no visible effect;
- re-enable cleanly restores the last valid choice.

Also stop `myna-desktop --dbus` during an ordinary active state and during each
held severity. An ordinary state clears to dormant. A recoverable notice keeps
only its original remaining time; a critical error remains until dismissal.
Restarting the service or switching style must not restart either held lifetime.

Repeat basic checks under high contrast and reduced motion. The bar remains
legible; reduced motion does not remove level information.

## 8. Structured state-identification trial

Using a stub publisher so every supported descriptor can be exercised, show at
least three observers each HUD style in loading, listening, transcribing,
finishing, recoverable-notice, and critical-error states in randomized order
without transcript content. Record correct/incorrect responses for all 36
observations (3 observers × 2 styles × 6 states).

**Expected**: at least 33/36 responses are correct (at least 90%), and no state
or style has a systematic misidentification. Record the anonymized aggregate and
any confused state pairs in this document when acceptance is run.

## 9. Offline acceptance

With the local backend and `myna-desktop --dbus` already running, disable all
network interfaces from GNOME Quick Settings (or disconnect Ethernet and Wi-Fi),
then complete one dictation session with each HUD and switch styles once during
recording. Do not stop the local session bus, PipeWire, IBus, or backend socket.

**Expected**: both HUDs, live switching, local transcription, and injection
continue to work; no remote fallback, network prompt, transcript log, or raw
audio file appears. Record the network-disabled state and result.

## 10. Desktop-session persistence

Select Wave ribbon, log out of the GNOME session, log back in, and verify Wave
ribbon remains selected. Repeat after selecting Basic.

**Expected**: the last valid preference survives each full desktop-session
restart, not only extension disable/enable. Record both results.

## 11. CI gate

The repository CI must run the Workshop GJS suite and package smoke on changes
to the extension. Rust/snap publisher tests remain unchanged because this feature
does not alter either boundary.

## Done when

- Headless GJS contracts and package smoke pass.
- Both preferences choices persist and switch live.
- Steps 4–7 pass in a real GNOME session without focus loss or duplicate/leaked
  presentation.
- The structured state-identification trial reaches at least 90% correct.
- Offline operation and both full-session preference persistence trials pass.
- Feature-004 wave acceptance remains unchanged when Wave ribbon is selected.
- No raw audio or transcript content appears in settings, HUDs, logs, or files.
