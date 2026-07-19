# Audio Adapter — meeting prep (T20 decisions)

**Date:** 2026-06-18
**Status:** Pre-meeting prep — refresh with decisions afterward
**Authors:** Claude, with Charles
**Context:** System architecture diagram in `docs/architecture/UD129 - Ubuntu Desktop STT Integration.md` (§ Architecture); agenda —
daemon-or-library, API with the dictation service, who handles the hotkey, …

In the diagram the **Audio Adapter** sits in the Speech Orchestrator between the
Dictation Service and the Audio Server (PipeWire), doing "audio preprocessing
(denoise, normalization, VAD), opens device, configures, sends frames." That is
plan **T20**. The thesis below: it is thinner than the box suggests, because the
two jobs people imagine for it (heavy DSP, VAD) mostly belong elsewhere.

---

## Framing: the adapter is thin *because* the scary jobs aren't its job

- **Heavy DSP (denoise, de-reverb, equalization) → audio server + model, not the
  adapter.** PipeWire/hardware already do echo-cancel, AGC, and noise
  suppression system-wide (the diagram lists "DSP" under the Audio Server). The
  ASR models are trained on real, noisy, reverberant speech — aggressive
  client-side cleaning adds artifacts they weren't trained on and can *raise*
  WER. The model is the de-reverb. Don't duplicate it upstream.
- **VAD → redundant for the MVP, and belongs near the model otherwise.**
  Push-to-talk makes the hotkey the VAD (hold = speech, release = commit). The
  diagram already assigns *segmentation* to the inference snap ("handles
  recognition and segmentation"), which is correct — segmentation needs acoustic
  context the adapter lacks. Same position we took against IE115 server-VAD
  (`docs/IE115-deviations.md` §1.4): in push-to-talk the client owns the boundary.

What's left is small and mechanical: **open the chosen PipeWire node → capture
into a bounded in-memory ring buffer (never persisted) → convert to exactly the
format the service advertised → push frames.** A library, not a subsystem.

**Honest caveat:** thin ≠ trivial. Three items carry WER landmines — resample
quality, input level/gain, and channel selection / mono downmix on odd hardware
(cf. the RME 94-channel example in IE114). Small surface, not thoughtless.

---

## Decision 1 — Daemon or library → **library, in-process with the Dictation Service**

**Recommendation:** library.

Rationale:
- **No independent lifecycle** — it only works while a session is active (key
  held); a daemon implies persistent state it doesn't have.
- **Latency** — audio is the hottest path (UD129: first event <500 ms). A daemon
  adds a process hop + another PCM serialization for zero isolation benefit
  (same user, same trust domain, same session).
- **Permissions** — the mic-opener holds the `audio-record`/portal grant.
  Library-in-the-service inherits the service's grant; a daemon needs its own
  grant and consent story (ties to T17).
- **Versioning** — a daemon boundary is a wire API that "must be versioned" (the
  diagram's own note); a library boundary is a typed interface, no wire, no
  negotiation. Pay versioning only where forced (orchestrator↔snap), not here.

Diagram ambiguity to clear up: the Audio Adapter is drawn as a *peer box* to the
Dictation Service, and **both** boxes claim to open the mic ("Dictation Service:
open the microphone stream" vs "Audio Adapter: opens device"). Boxes ≠
processes: the adapter is a module *inside* the service; the adapter opens the
device, the service requests a stream from it.

**Decision (post-meeting):** _TBD_

## Decision 2 — API with the dictation service → **reuse `AudioSource` → `PcmChunk`**

**Recommendation:** don't invent a new API; the existing `myna.core`
`AudioSource` protocol *is* this interface — the service consumes an async
`PcmChunk` stream, the adapter produces it.

- Makes the live-mic adapter, the WAV source, and the fake source
  interchangeable — same contract-test discipline as the transport.
- `dev/dictate.py`'s `MicSource` (`pw-record` → `PcmChunk`) is the working seed;
  T20 hardens it (bounded ring buffer, device/channel selection, conversion).
- Format flows *down*: service queries capabilities → picks an `input_format` →
  configures the adapter → adapter captures + converts → pushes. **This is where
  T33 lands** — the diagram's "PCM / float32(?)" question mark is exactly the
  open int16-vs-float32 decision, and the adapter is the component that does the
  conversion.

**Decision (post-meeting):** _TBD_

## Decision 3 — Who handles the hotkey → **two layers; don't conflate**

**Recommendation:**
- **Grab/registration → compositor/portal, not us.** On Wayland, apps can't grab
  global keys; go through GNOME or the `xdg-desktop-portal` GlobalShortcuts
  portal. The Settings UI's "Customize hotkey" binds it there.
- **Reaction → the Dictation Service** (drives the session state machine, T21).
  The diagram's placement is right for the reaction.

This couples three agenda items that look independent:

> **hotkey mechanism → interaction model (hold vs toggle) → whether you need VAD.**

**Resolved (source-verified 2026-06-18, GNOME 50 — xdg-desktop-portal-gnome
50.0, mutter 50.1, gnome-shell 50.1):** hold-to-talk **is** supported through
`org.freedesktop.portal.GlobalShortcuts` — the client gets both `Activated`
(press) and `Deactivated` (release). The portal backend grabs every accelerator
with `META_KEY_BINDING_TRIGGER_RELEASE`
(`xdg-desktop-portal-gnome/src/globalshortcuts.c:168`), which propagates through
gnome-shell `GrabAccelerators` (`shellDBus.js:322`) to mutter, whose dispatch
runs a release handler when that flag is set (`keybindings.c:1505`) and emits
`accelerator-deactivated` → portal `Deactivated`. It is **upstream**, not an
Ubuntu patch (`debian/patches/series` doesn't touch shortcuts). So: **no GNOME
Shell extension needed, and VAD stays out of the MVP** — the service reacts to
`Activated`=start / `Deactivated`=stop.

Two caveats, both now settled:
- **Modifier-only key — no** (confirmed by Marco/3v1n0, GNOME expert): we can't
  grab a bare modifier, and it would be *wrong* — modifiers carry meaning and
  would break apps (e.g. Ctrl drives the find-mouse-pointer a11y feature). So the
  PTT key is a **normal key or chord**, not a held modifier.
- **Autorepeat:** the grab sets `TRIGGER_RELEASE` but not `IGNORE_AUTOREPEAT`
  (`keybindings.c:1468`), so a held key may repeat `Activated`. Handle
  client-side: first `Activated` = start, ignore until `Deactivated`. Non-issue.

**Default-key choice (in progress with Marco):**
- Hold-to-talk wants the *fewest* keys to hold comfortably → a 2-key
  `Super+<letter>`, not an awkward `Super+Shift+<letter>`.
- Mirror Windows where free, avoid where reserved — but note both Windows
  mnemonics are taken in GNOME: **Super+H = minimize** (Windows' dictation key),
  **Super+V = notifications toggle** (and Windows clipboard; Marco is holding V
  for a future Ubuntu clipboard manager).
- Pick a **free** `Super+<letter>` as the default; mnemonic is secondary because
  the portal `BindShortcuts` carries a `preferred_trigger` the user confirms /
  rebinds in Settings (the "Customize hotkey" flow). Default only needs to be
  "free + unsurprising".
- **To do:** enumerate taken combos authoritatively on a resolute box
  (`gsettings list-recursively | grep -iE "'<Super>"`) — the full set lives in
  `gsettings-desktop-schemas`, not in the probe tree — and ask Marco which other
  letters he's reserving for future GNOME/Ubuntu features before fixing a default.

**Recommendation:** hold-to-talk via the portal, on a free 2-key `Super+<letter>`
default, rebindable in Settings. Toggle stays a trivial fallback (same
`Activated`, ignore `Deactivated`).

**Open UX question for the meeting:** does the "Customize hotkey" Settings flow
set `preferred_trigger` and let the user rebind via the portal's shortcuts UI?
(Asked Marco.)

**Decision (post-meeting):** _TBD_

## The "…" — pre-empt so they don't derail

- **Mic permission / consent (T17):** the adapter is the mic-opener, so the
  consent story lands on its host process. IE114's polkit / per-session-prompt
  thread is unresolved.
- **Device change mid-session:** IE114 comments [f][w] — disconnect-and-wait vs
  hold-stream-open when the user swaps mic. The adapter owns device selection;
  this is a policy call.
- **No-persistence invariant:** bounded in-memory ring buffer, discarded on
  session end. Say it out loud — "audio adapter" tempts people toward
  buffer-to-disk.
- **Post-processing boundary:** the diagram cleanly separates Text Finalization
  (orchestrator) from the snap — affirm it. Matches IE114's conclusion and our
  T23 that punctuation/capitalization/cleanup is the consumer's job, not the STT
  service's. Don't let it leak into the adapter or the snap.

---

## Bottom line to advocate

Audio Adapter = a **library** inside the Dictation Service, exposing the existing
**`AudioSource` → `PcmChunk`** interface, whose only real job is **device/channel
selection + bounded buffering + format conversion to the negotiated
`input_format`**. DSP stays in PipeWire and the model; VAD/segmentation stays
near the model and is out of the MVP; the hotkey is grabbed by the
compositor/portal and *reacted to* by the service — and settle hold-vs-toggle
first, because it decides whether VAD is even on the table.

---

## Post-meeting outcomes

_Refresh after the meeting: record decisions, attendees, and any new open
questions to fold into the plan (Workstream D / T20–T22, T33, T17)._
