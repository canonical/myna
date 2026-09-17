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
- `myna` installed (snap) or built from `client/` with `--features ui-gtk` for
  the `GtkIndicator` scenarios.
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

1. Set text scale to 200%, enable the high-contrast theme, enable
   forced-colours (if available), enable reduced-motion.
2. Run a full dictation session, observing the HUD pill through each state.
3. **Expect**: all text remains fully visible, unclipped; the live-capture
   indication has a static equivalent under reduced motion; nothing flashes
   more than 3×/second.

## Scenario 6 — Sound cues do not degrade transcription (US3, SC-006)

Run via the existing real-corpus WER benchmark harness
(`dev/fetch_real_corpus.py` + the project's WER measurement tooling) twice:
once with sound cues/announcements enabled, once with both disabled (silent
baseline). Compare WER delta — must be ≤0.5 percentage points.

## Scenario 7 — Confined package (FR-033, SC-009)

Repeat Scenarios 1–3 against the installed strictly-confined `myna` snap
(not a dev build), confirming the accessibility bus connection succeeds under
confinement (snapd's `desktop` plug) with no additional manual `snap connect`
beyond what's already documented for notifications/portals.

## Scenario 8 — Terminal client under a screen reader (US5, SC-008)

1. Run `myna-dictate` in a terminal with Orca's terminal/console support
   active, colour disabled (`NO_COLOR=1` or non-tty redirection observed
   separately).
2. Dictate a session; observe that state changes are read as discrete new
   lines, not repeated re-reads of a redrawn line.
3. Force a failure; confirm it appears on stderr in the same plain language as
   Scenario 2.
