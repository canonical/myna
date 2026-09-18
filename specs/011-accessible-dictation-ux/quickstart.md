# Quickstart / Manual Acceptance Protocol

Automated gates (hermetic + `MYNA_ATSPI_TESTS`/`MYNA_PIPEWIRE_TESTS`-gated) cover
everything listed in the contracts under `contracts/`. FR-031 additionally
requires a documented **manual** protocol for the properties that cannot be
verified headlessly (research.md R6). Run this against a real GNOME session —
ideally the packaged, strictly confined snap (FR-033/SC-009), with a fallback
run against the unpackaged dev build noted where they'd diverge.

## Prerequisites

- A GNOME Wayland session with Orca (or another AT-SPI-consuming screen
  reader) installed and running.
- `myna` installed (snap) or built from `client/`. (Earlier drafts asked for
  `--features ui-gtk` for `GtkIndicator` scenarios; that overlay was removed,
  so there is no such feature and no such scenario.)
- `extensions/myna-shell` installed and enabled (`gnome-extensions enable
  myna-shell@...`).
- A text field to dictate into (e.g. GNOME Text Editor).

## Scenario 1 — Full dictation with the screen off (SC-001, US1)

1. Start Orca. Turn the display off (or point away).
2. Trigger dictation (tap-to-start).
3. **Expect**: "listening" announced within 500 ms, no focus movement.
4. Speak a short sentence. End the session (tap-to-stop).
5. **Expect**: "transcribing" then a completion/insertion confirmation are
   announced, in that order, with no stale/duplicate announcements.
6. Query the indicator state via the AT's "where am I" command at a moment
   between transitions.
7. **Expect**: current state is reported accurately (FR-001).
8. **Braille check (Acceptance Scenario 3, T044)**: repeat steps 1–5 with a
   braille display connected and Orca's braille output enabled instead of (or
   alongside) speech. **Expect**: the same content-free state text (e.g.
   "Listening", "Finishing") reaches the braille device — no separate braille
   code path exists or is needed, since both `AtspiAnnouncer` (Rust) and
   `Announcer` (GJS) emit the standard AT-SPI `Announcement` event
   (`org.a11y.atspi.Event.Object`), which Orca routes to speech and braille
   identically. No code change was required to satisfy this scenario; it is
   verified here rather than assumed, per FR-002's "reaching both speech and
   braille through the same path".

## Scenario 2 — Failure without sighted assistance (US1, US4, SC-007)

1. Stop the inference backend.
2. Trigger dictation.
3. **Expect**: within 10 seconds of the failure, the AT announces a
   plain-language message naming the problem and the next action — no error
   codes or internal names.
4. Repeat while watching the indicator, the notification, and (separately) run
   `myna-dictate` in a terminal for the same failure.
5. **Expect**: all three surfaces plus the announcement convey the same
   meaning and recovery action (contracts/failure-mapping.md F2).

## Scenario 3 — No pointing device (US2, SC-004)

1. Disconnect/disable the pointer (or use only keyboard/switch input).
2. Start dictation via a single bound key (no chord, no hold).
3. Speak a multi-sentence utterance; end by falling silent (no second input).
4. **Expect**: text is inserted; at no point was a held or chorded key
   required.
5. Trigger a critical error (e.g. no microphone) and dismiss/acknowledge it
   using only non-pointer input.
6. **Expect**: acknowledgement succeeds with no pointer/hover interaction.
7. **Switch-access / dwell-click check (T052, FR-020)**: enable GNOME's
   built-in switch access (Settings → Accessibility → Switch Access) or
   pointer dwell-click (Settings → Accessibility → Pointing & Clicking →
   Click Assist → Hover Click), then activate the same bound dictation key/
   control using only that assistive input method — a switch-access scan
   selecting the key, or a dwell-click landing on the target — instead of a
   literal keypress or a literal pointer click.
8. **Expect**: the session starts identically to step 2/3, with no
   Myna-specific accommodation, prompt, or special mode required. **Finding
   (code audit, T052)**: confirmed by reading `client/myna-desktop/src/
   shortcut/portal.rs`'s `bind()`/`Dedup` and `client/myna-desktop/src/
   shortcut/control.rs` — `myna-desktop` has no code path that inspects or
   special-cases *how* an activation event was produced. The portal path
   only ever receives the compositor's `Activated`/`Deactivated` D-Bus
   signals for the bound shortcut (switch access driving a key scan
   ultimately delivers the same physical/synthesized key event the
   compositor turns into those signals); the control-socket path only ever
   sees a Unix-socket connect from `myna-desktop --toggle` (a dwell-click on
   a launcher/icon bound to that command is indistinguishable from a literal
   click). Neither trigger implementation has, or needs, any notion of
   "assistive input method" at all — this is a structural, not incidental,
   property of the design (FR-020 requires no additional accommodation, and
   none exists to remove).

## Scenario 4 — Sticky keys / slow keys / autorepeat (US2)

1. Enable sticky keys in GNOME's accessibility settings (Settings →
   Accessibility → Typing → Sticky Keys). Activate the dictation shortcut
   once (a single key, pressed and released normally — sticky keys' own
   sequential-modifier behavior does not apply to a single-key binding, but
   this confirms it doesn't interfere).
2. **Expect**: exactly one session starts; the session ends cleanly on the
   next tap (or silence) with no dropped or duplicated edge (i.e. it doesn't
   silently need a second tap to actually stop, and it doesn't stop on its
   own before the user's second tap).
3. Disable sticky keys; enable slow keys instead (Settings → Accessibility →
   Typing → Slow Keys), which adds an acceptance delay before a keypress
   registers.
4. Repeat step 1's single activation under slow keys.
5. **Expect**: exactly one session starts once the slow-keys acceptance delay
   elapses (no extra latency-induced duplicate activation, no dropped
   activation if held past the acceptance threshold).
6. With sticky keys or slow keys still enabled, physically hold the bound
   key down long enough for the OS/compositor's own key autorepeat to fire
   (do not tap-tap — hold continuously).
7. **Expect**: still exactly one session starts (autorepeat's rapid
   `Activated` signals collapse to a single edge — `Dedup`'s dedup logic,
   `client/myna-desktop/src/shortcut/portal.rs`, hermetically regression-
   tested by `toggle_hold_does_not_stop_the_session` and
   `large_autorepeat_burst_still_yields_a_single_toggle_edge`); releasing the
   key does not produce a second, spurious edge.
8. **Note**: this scenario cannot be simulated hermetically — GNOME's sticky-
   keys/slow-keys mediation and the resulting key-event timing happen in the
   compositor/input stack, below anything `myna-desktop` observes (it only
   ever sees the portal's already-mediated `Activated`/`Deactivated`
   signals) — so steps 1–7 remain a required manual verification, not a
   candidate for a unit test.

## Scenario 5 — Large text / high contrast / reduced motion / forced colours (US3, SC-005)

Reads (T059): `client/myna-desktop/src/preferences.rs`/`extensions/myna-shell/accent.js`'s
`SystemPreferences` class watches `org.gnome.desktop.interface`'s
`text-scaling-factor` and `org.gnome.desktop.a11y.interface`'s `high-contrast`
(the real GNOME 47+ key — not the old `gtk-theme` "HighContrast" string hack),
alongside feature 004's existing `enable-animations` (reduced motion) and
`accent-color` reads. **GNOME has no separate "forced colours" toggle**
distinct from `high-contrast` (that is a Windows/CSS-media-feature concept);
this scenario's "forced colours" step is the same GNOME high-contrast theme,
not an additional setting — do not look for one.

1. `gsettings set org.gnome.desktop.interface text-scaling-factor 2.0` (200%).
2. `gsettings set org.gnome.desktop.a11y.interface high-contrast true`
   (enables the GNOME high-contrast GTK theme; this is also GNOME's "forced
   colours" equivalent — see note above).
3. `gsettings set org.gnome.desktop.interface enable-animations false`
   (reduced motion).
4. Run a full dictation session, observing the HUD pill through each state
   (Listening → Finishing → a completion/error notice → Idle).
5. **Expect**:
   - At 200% text scale, every HUD/notification label remains fully visible
     and unclipped — no truncated or overlapping text.
   - Under the high-contrast theme, the pill/notification chrome adopts the
     theme's contrast palette (via normal GTK/St theming — the pill draws
     with themed colours, not hardcoded ones) and remains legible.
   - Under reduced motion, the live-capture indication (the wave ribbon, or
     the GTK indicator's pulsing) is replaced by its static equivalent
     (`resolveReducedMotion`/feature 004's existing contract X26) — nothing
     animates, and nothing flashes more than 3×/second (WCAG 2.3.1).
6. Restore defaults: `gsettings reset org.gnome.desktop.interface
   text-scaling-factor`, `gsettings reset org.gnome.desktop.a11y.interface
   high-contrast`, `gsettings reset org.gnome.desktop.interface
   enable-animations`.

## Scenario 6 — Sound cues do not degrade transcription (US3, SC-006)

**Architectural note, checked before running anything**: `sound::SoundCuePlayer`
(T054/T057) plays cues on their own, independent PipeWire *output* stream
(`Direction::Output`, a fresh short-lived connection per cue — see
`client/myna-desktop/src/sound/playback.rs`'s module docs) and has no code
path into the *capture* stream the ASR pipeline consumes — `controller.rs`'s
`self.sound.play(..)` calls (T058) are side calls alongside the existing
indicator/injector calls, never touching `myna_audio`'s capture buffer.
The only physically possible way a sound cue could affect a transcript is
acoustic: the cue audibly playing through speakers and being picked up again
by an open microphone in the same room (an environment/hardware condition,
not a myna code path) — which is why this is a **live acoustic check**, not
a candidate for the existing corpus-based WER harness
(`dev/bench.py`/`dev/fetch_real_corpus.py`): that harness feeds pre-recorded
clip files directly into the backend over the ASR socket and never opens a
live microphone or a live speaker, so it cannot exercise (or catch a
regression in) this acoustic path at all — running it with cues "enabled"
vs. "disabled" would measure nothing.

1. On real hardware with a working microphone and speakers (not headphones —
   the acoustic path only exists when playback can reach the mic), leave
   `sound-cues-enabled` at its default (`true`).
2. Start `myna-dictate` (or the desktop daemon) and read the same fixed
   passage aloud for two runs: once as normal, once immediately after a
   `SessionStart`/`StopListening`/`SessionEnd`/`Failure` cue has audibly
   played (e.g. trigger a deliberate failure first — an empty focused field
   — to hear the `Failure` cue, then dictate the passage).
3. **Expect**: the two transcripts are identical (allowing for normal
   run-to-run speech variance) — the cue tones (fixed sine tones at
   880/1046/660/220 Hz, T057) do not appear as spurious words/fragments in the
   transcript, and no run is measurably slower to start capturing than the
   other.
4. **Not run as an automated corpus benchmark**: per the architectural note
   above, `dev/bench.py`'s existing WER harness bypasses both live capture
   and live playback, so it cannot observe this acoustic path either way —
   recording a WER delta from that harness would not be a real measurement
   of this scenario's risk (spurious/fabricated data), so none is recorded
   here. If a future change makes the acoustic path harder to reason about
   (e.g. a persistent/looping cue, or genuine echo-cancellation coupling
   with `myna-audio` capture), a live-hardware acoustic protocol like the
   one above — not `dev/bench.py` — is the correct tool to reach for.


## Scenario 7 — Confined package (FR-033, SC-009)

Repeat Scenarios 1–3 against the installed strictly-confined `myna` snap
(not a dev build), confirming the accessibility bus connection succeeds under
confinement (snapd's `desktop` plug) with no additional manual `snap connect`
beyond what's already documented for notifications/portals.

## Scenario 8 — Terminal client under a screen reader (US5, SC-008)

1. Run `myna-dictate` in a terminal with Orca's terminal/console support
   active, colour disabled: `NO_COLOR=1 myna-dictate --socket /tmp/myna.sock
   --mic`.
2. Dictate a session; observe that every state/result line begins with an
   explicit `[marker]` (e.g. `[loading]`, `[ready]`, `[committed]`,
   `[done]`) — meaning survives even with the decorative emoji ignored
   (FR-028). With `NO_COLOR=1` set, the live VU meter (otherwise redrawn
   in-place on one line with `\r`) is suppressed entirely rather than
   printing a fresh line per audio-stats update — the alternative
   (line-per-update) would itself spam a screen reader with far more
   frequent re-reads than the meter is worth (FR-029).
3. **Expect**: state changes are read as discrete new lines, not repeated
   re-reads of a redrawn line; without `NO_COLOR`, the sighted VU meter
   still redraws in place as before (that path is unaffected — this
   scenario is specifically about the `NO_COLOR`-requested mode).
4. Force a failure (e.g. stop the inference backend, or dictate into
   `--clip` pointing at a nonexistent file); confirm it appears on stderr
   prefixed `[error]`, in the same plain language as Scenario 2 — the exact
   same `FailurePresentation` text, not a separate CLI-only wording
   (contracts/failure-mapping.md F2, contracts/terminal-output.md T3).
