# Preface

Read this document when changing desktop activation, focus handling, IBus injection, preedit, notifications, or the HUD.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

`myna-desktop` composes three replaceable boundaries around the orchestrator:

- `Trigger` produces activation edges. Unpackaged GNOME uses a control socket and custom shortcut; packaged builds can use the GlobalShortcuts portal; stdin is debug-only.
- `Injector` commits text to the focused application. The production implementation is an IBus engine; tests use a mock.
- `Indicator` publishes dictation state. Notifications are the fallback, while the GNOME extension hosts the standalone `myna-hud` renderer.

The controller acquires the target before capture, refuses known secure fields, starts capture on activation, and finalizes on release or focus loss. Text buffered when focus is lost is discarded rather than sent to a different target.

Committed segments may be coalesced before IBus insertion because rapid adjacent commits are not reliable across all targets. Natural whitespace from the backend is preserved; compatibility spacing is added only when neither boundary supplies whitespace.

In streaming mode, unstable hypotheses may replace the target's preedit region when the injector supports it. Preedit is volatile, clears before commits and on cancellation, and follows the same secure-field and focus-loss guards as committed text.

# Important

- Never inject unstable hypotheses with `CommitText`.
- Never inject into a known secure field or after focus has moved.
- Do not let the indicator or HUD take keyboard focus.
- Keep activation, injection, and indication behind mockable traits.
- Treat source and tests in `myna-desktop` as authoritative for IBus serialization details.
