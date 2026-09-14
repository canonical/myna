# Preface

Read this document when changing desktop activation, focus handling, IBus injection, preedit, notifications, or the HUD.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

`myna-desktop` composes three replaceable boundaries around the orchestrator:

- `Trigger` produces activation edges. Unpackaged GNOME uses a control socket and custom shortcut; packaged builds can use the GlobalShortcuts portal; stdin is debug-only.
- `Injector` commits text to the focused application. The production implementation is an IBus engine; tests use a mock.
- `Indicator` publishes dictation state. Notifications are the fallback, while the GNOME extension hosts the standalone `myna-hud` renderer.

The controller acquires the target before capture, refuses known secure fields, starts capture on activation, and finalizes on release or focus loss. Text buffered when focus is lost is discarded rather than sent to a different target.

Toggle activation has no release edge, so the controller also ends a session by policy (`AutoStop`): after the user's silence timeout without sustained voice, measured in captured audio from the stats tap, and in any case at a fixed session cap. Both end exactly like a release, plus the trigger-parity resync a focus loss needs. Hold-to-talk and the debug stdin trigger run with the policy off.

On completion the controller classifies the input from the same tap (`input_quality`): a noise floor above -50 dBFS, or speech under 15 dB above it, raises the recoverable "Background noise is high" notice on every such session. An empty transcript keeps its own message. Thresholds are prototype calibration from the HUD meter's headset baseline; the remedy stays in the PipeWire graph.

Committed segments may be coalesced before IBus insertion because rapid adjacent commits are not reliable across all targets. Natural whitespace from the backend is preserved; compatibility spacing is added only when neither boundary supplies whitespace.

In streaming mode, unstable hypotheses may replace the target's preedit region when the injector supports it. Preedit is volatile, clears before commits and on cancellation, and follows the same secure-field and focus-loss guards as committed text.

# Why an input method, not emulated input

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
- Do not let the indicator or HUD take keyboard focus.
- Keep activation, injection, and indication behind mockable traits.
- Treat source and tests in `myna-desktop` as authoritative for IBus serialization details.
