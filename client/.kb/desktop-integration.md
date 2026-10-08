# Preface

Read this document when changing desktop activation, focus handling, IBus injection, preedit, notifications, the session sounds, or the HUD.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

`myna-desktop` composes three replaceable boundaries around the orchestrator:

- `Trigger` produces activation edges: every poke toggles. A GNOME custom shortcut calls `com.canonical.Myna.Dictation.Toggle`; `myna.toggle` connects to the control socket instead, for shortcuts older Myna Settings wrote, and both feed one channel with one parity. Stdin is debug-only.
- `Injector` hands out one `Target` per utterance, the sole owner of the right to write into the focused application. The contract is `myna_platform::text_input` (`.kb/platform-layer.md`), and the controller names nothing else of text input: no IBus type, only the contract and the capabilities a backend reports. The production implementation is an IBus engine; tests use a mock that models the field it writes into.
- `Indicator` publishes dictation state. Notifications are the fallback, while the GNOME extension hosts the standalone `myna-hud` renderer; on X11 window managers the renderer hosts itself (`myna-hud --host x11`, `.kb/platform-layer.md`).

The key is a GNOME custom shortcut Myna Settings writes (`myna-config/docs/onboarding.md`). The GlobalShortcuts portal was the packaged activation until 2026-10 and was removed with hold-to-talk: stacked and duplicate consent dialogs, consent and retry races, app id drift (`myna_myna` versus `.`), grants surviving `snap remove --purge`, grabs needing a re-login, and no GlobalShortcuts at all on Noble's portal. `Activation` is still published, always `control`, for Myna Settings releases that read it.

The controller acquires the target before capture, refuses known secure fields, starts capture on activation, and finalizes on release or focus loss. Text buffered when focus is lost is discarded rather than sent to a different target. One select loop covers the whole utterance, including the events still queued after the session future finishes, so a focus loss is always seen before the next queued segment is written; the trigger and stats arms shut off once the session ends, because an edge arriving then belongs to the next utterance. A write the target refuses because its lease is gone ends the utterance as well: nothing further is attempted, and because that text reached no field, the completion reports the loss rather than a clean insertion, whatever the transcript holds. An empty transcript after a focus loss reads the same way rather than as silence.

A capture fault mid-utterance does not end the utterance at the controller either. The orchestrator finishes the session with the audio it had already accepted and raises the device afterwards, so the controller sees `CaptureLost` while the transcript is still coming: it moves the indicator to finishing, since capture is over even though the utterance is not, keeps the message, inserts the committed text exactly as usual, and then shows that failure where the completion notice would go. The HUD reads recording, finishing, error, and never shows the hidden or "No speech detected" completion on the way, which would claim a clean dictation a dead microphone did not give. A fault with nothing captured has no text to wait for and stays an immediate error.

A target writes under a lease minted before the engine switch and bound to the input context the daemon focuses the engine on. Focus leaving that context, focus reaching any other context, a newer lease, or release ends it. Until focus arrives, the lease ignores what the previous release is still delivering: its `FocusOut`, and the `FocusInId` on the daemon's fake context that follows every `FocusOut` (named by the client string `fake`, not by its path, which looks like any other). Neither can be this lease's, because the daemon sends no `FocusOut` before the first `FocusIn` of an activation and never relays a focus change it handled before our engine was attached; nothing orders their dispatch against the next mint, so ignoring them is what keeps a good press from reporting a focus loss or binding the fake context. Once the lease is focused, both end it. Commit and preedit refuse an ended lease, and a focus stream taken afterwards still reports the loss. `acquire` is all or nothing within one limit: the rollback restores whatever engine the connection displaced, so where it displaced none, `myna-stt` stays the global engine, IBus having no call to unset one. The displaced input method belongs to the connection, not to one utterance: an activation records the engine `GetGlobalEngine` named, unless that is already `myna-stt`, where an earlier activation is still standing and what it displaced is kept instead. `myna-stt` is never recorded as something to restore, by any path. Release retires the lease before restoring, whose switch focuses the engine out; the release that still held the field, or that merely lost focus, consumes the record, and a superseded one restores nothing, because the lease that displaced it in turn carries the responsibility on. A failed restore is not retried.

The lease binds an input context, not an application. GNOME Shell may multiplex every application through a single IBus input context, which is unverified here; where it does, context identity discriminates nothing between applications and the protection rests on the Shell sending `FocusOut` as focus moves. The lease check narrows the hole rather than closing it in any case: between `commit` reading the lease and the daemon routing `CommitText` to whichever context is focused when it arrives, the window is bounded, not eliminated, because IBus offers no way to address a commit to one context.

The engine exposes `FocusId` and `ActiveSurroundingText` so ibus-daemon names the context in `FocusInId`/`FocusOutId`. The daemon reads both asynchronously and caches the answer in a table it never clears, keyed by a pointer into the engine description, so any activation may still start with a plain `FocusIn`; once it has read `ActiveSurroundingText` it re-sends that focus as `FocusInId`, which names the lease's context. A lease that sees no focus while acquiring ends at the next focus call. The engine interface is dispatched without per-call tasks: zbus spawns one per method call by default, and focus calls must apply in the order the daemon sent them.

`ActiveSurroundingText` is true, so on every focus the daemon asks the field for the text around its cursor (on GNOME Wayland the Shell answers from text-input-v3). The engine keeps only the character before the cursor, or before the selection a commit replaces, for the focused lease, and never logs the text. The first commit of a dictation gets a space after a character that wants one, so a second dictation does not run on from the first; there is none at a line start, after an opening bracket, before closing punctuation or next to Chinese, Japanese or Thai. A field that sends no surrounding text gets no space. A daemon that read `false` from an older Myna keeps that until it restarts, which a re-login does.

A field is secure when its purpose is PASSWORD or PIN, or its hints include HIDDEN_TEXT; the guard runs at acquire, at every commit and for preedit. ibus-daemon delivers content type only by setting the engine's write-only `ContentType (uu)` property, after FocusIn, and this holds on GNOME Wayland, where the Shell forwards text-input-v3 content type. GNOME has no PIN purpose mapping, so a PIN field arrives as purpose 0 with PRIVATE and HIDDEN_TEXT, which the HIDDEN_TEXT rule catches. PRIVATE alone is accepted because browsers set it on every private-window field. A backend that reports `secure_field_detection: Unknown` is dictated into anyway (decision D4), as an IBus field that sends no content type is; the controller's `dictates_into` is the one place that policy is set.

While the `sounds` setting is on, the session is heard as well as seen. `sound::Chiming` wraps the daemon's indicator and derives the cues from the states it is shown, so a cue can never disagree with the HUD: Start when listening begins, which is the press and not the model's `Ready`, since capture buffers the speech through a cold load (a press with no model to reach therefore chimes Start then Error), Stop once per session at the first of finishing (`Finalizing`) or the end (hidden, or a recoverable notice such as "No speech detected"), and Error on a critical error, even after Stop, but not for a repeat of the error already showing; a press refused before capture is an Error alone. The cues are Myna's own, seven sets of start, stop and error compiled into the daemon from `myna-desktop/sounds/<set>/` and synthesized by `dev/synth_cues.py`: `myna` (Bird) whistled chirps with an alarm churr for error, `tine` (Chime, the default) a struck bar by modal synthesis, `hum` (Voice) the rising "hm?", falling "mm-hm." and clipped "uh-uh" of spoken backchannels, `marimba` (Marimba) the first cues Myna shipped, `drop` (Water) water drops whose bubbles rise in pitch as real ones do, `radio` (Radio) a walkie-talkie's squelch, two-tone and roger beep, band-limited to 300 Hz - 3.4 kHz, with mistuned beating carriers for error, and `koto` (Strings) Karplus-Strong plucks with a sitar-bridge buzz for error. The `sound-set` setting picks one, read at each cue. Within a set, stop is 2 LU quieter than start and error 1 LU louder, matched by K-weighted loudness rather than peak (a test measures it: radio peaks well below the struck sets at the same loudness), and every error differs from its start and stop in register, roughness and rhythm. The marimba set was the only one at first and was briefly dropped: each note was cut off 90 ms before the clip ended while still ringing, a click in every cue. It returned with each note released to zero; its 220 Hz error still falls below what laptop speakers reproduce. Theme events were tried first and dropped: no freedesktop event means "listening", and `device-added`/`device-removed` read as a USB drive. Rising-for-on is the convention every dictation product shares. The clips must fade in and out to silence, as a clip that stops above zero clicks; a test pins it. `Player` decodes the Ogg Vorbis clip (`myna_audio::playback`, at most 3 s) and plays it through a native PipeWire output stream on a thread of its own, one cue at a time with a short backlog, and gives up shortly past the cue's length when nothing plays it. Native protocol clients are not subject to WirePlumber's snap audio policy, which only the pulse protocol labels, so the `pipewire` plug that captures also plays and no `audio-playback` plug is needed. A failed play is logged and dropped. The start cue plays as capture opens, so the microphone can hear it. `PreviewSounds` on the D-Bus interface plays a whole set (start, stop, error, 500 ms apart) on the same player thread for Myna Settings' Preview button: the cues live in the daemon, so it is the one that plays them, and it alone knows a session is under way, so it refuses from the press until the session ends (state other than idle, notice or error), as the microphone would dictate the preview. A second preview while one plays is refused rather than queued.

Toggle activation has no release edge, so the controller also ends a session by policy (`AutoStop`): after the user's silence timeout without sustained voice, measured in captured audio from the stats tap. It ends exactly like a release, plus the trigger-parity resync a focus loss needs. The debug stdin trigger runs with the policy off. There is no session length cap: a model with an input limit is the backend's to window.

On completion the controller classifies the input from the same tap (`input_quality`): a noise floor above -50 dBFS, or speech under 15 dB above it, raises the recoverable "Background noise is high" notice on every such session. An empty transcript keeps its own message. Thresholds are prototype calibration from the HUD meter's headset baseline; the remedy stays in the PipeWire graph.

Committed segments may be coalesced before IBus insertion because rapid adjacent commits are not reliable across all targets. Natural whitespace from the backend is preserved; compatibility spacing is added only when neither boundary supplies whitespace.

In streaming mode, unstable hypotheses may replace the target's preedit region when the injector supports it. Preedit is volatile, clears before commits and on cancellation, and follows the same secure-field and focus-loss guards as committed text.

## Why an input method, not emulated input

The cross-desktop direction for synthetic input is libei/libeis mediated by the
`org.freedesktop.portal.RemoteDesktop` portal, and Mutter already carries libei
support. But libei emulates a *device* - keycodes and pointer motion - while
dictation needs a semantic text commit. The difference is substantive, not
cosmetic:

- A keycode is not a character. It names a physical key position, and which
  character it produces is decided later by whatever xkb layout the *receiving*
  client has at delivery time. Emitting "ü", "€", or anything Cyrillic means
  finding a (keycode, modifier level) pair that maps to that keysym in the
  layout in force, and temporarily rewriting the keymap when none exists - the
  trick `wtype` and `ydotool` use. That races the user's own typing and layout
  switching, and per-window layouts make it unwinnable. A commit is a UTF-8
  string: layout never enters the picture.
- Modifier state is global and shared. Synthesising capitals or AltGr levels
  means pressing and releasing real modifiers on the seat, interleaved with
  whatever the user physically holds down. Lose that race and the text is
  delivered as accelerators; a sentence typed while Ctrl is latched is a run of
  commands, some destructive.
- Keystrokes get interpreted; commits get inserted. Every synthetic key
  traverses the app's key handling: autocomplete popups, type-ahead find, modal
  editors, key repeat, candidate windows. A 120-character utterance is 120
  opportunities to intercept, reorder, or transform it. A commit arrives on the
  text-input interface as one atomic string the widget inserts verbatim, and
  typically as a single undo step.
- Keycodes fight the user's real IME; a commit sits downstream of it. With a
  CJK or Hangul input method active, synthetic keys feed *its* composition
  engine, so "nihao" becomes a candidate lookup rather than text. Being an input
  method puts our output exactly where an IME's committed text goes, after
  composition, and makes handing focus back to the user's IME a protocol
  operation rather than a guess.
- Streaming ASR needs preedit, which only the IM path has. `text-input-v3`
  carries provisional preedit alongside commit, plus surrounding-text and
  delete-surrounding-text. That is the exact shape of incremental recognition:
  show the running hypothesis as styled preedit, replace it when the recogniser
  revises, commit once it is stable. Keycode emulation can only fake revision
  with backspaces, which is lossy against any field that reflows, autocorrects,
  or autocompletes underneath it.
- Delivery is scoped. The compositor routes a commit to the surface holding an
  active text-input, so it can only land somewhere that asked for text.
  Emulated keys go wherever focus happens to be at delivery, including a
  different window if focus moved mid-utterance.

There is no cross-desktop text-commit portal yet, so IBus stays the GNOME path
and `input-method-v2` the wlroots path. The genuinely-unsettled question is
whether a portable IM/text-injection interface ever standardises; until then
`Injector` is the portability boundary and IBus is the shipping backend.

# Important

- Never inject unstable hypotheses with `CommitText`.
- Never inject into a known secure field or after focus has moved.
- Implement what ibus-daemon sets or reads on an engine as D-Bus properties, and check declarations against a real engine's introspection (e.g. `gdbus introspect --address "$(ibus address)"` on `ibus-engine-simple`): the daemon discards error replies, so a wrong declaration fails silently.
- Do not let the indicator or HUD take keyboard focus.
- Keep activation, injection, and indication behind mockable traits.
- Treat source and tests in `myna-desktop` as authoritative for IBus serialization details.
