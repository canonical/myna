# Contract: `com.canonical.Myna.Dictation` (session bus)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; architecture revision: 2026-08-26)

The single seam between `myna-desktop` (publisher, Rust/`zbus`) and the dictation
indicator. **(2026-08-26)** The consumer is now the renderer application
(`myna-hud`, Rust) rather than the GNOME Shell extension — the extension no
longer holds any `com.canonical.Myna.Dictation` proxy; every guarantee below is unchanged
and simply re-homed. State + level only — never transcript content (constitution
V). This contract is defined here and encoded as executable tests on both sides
before implementation. Fallback suppression uses `RegisterClient` client set (C14/C15).

The `C-` numbering below is **wire-level**: it is enforced by both the
publisher (`myna-desktop`) and any consumer that wants to be a correct
client. The renderer-side consumer lifecycle (dormancy on no owner,
reflect-on-appeared, clear-on-vanished, no-leak disable, re-enable) was
previously referenced here as `X7–X10` — it has moved to the **renderer
contract** (`RC7–RC10`, in `extension.md`'s Renderer Contract section) since
those guarantees are now an attribute of the `myna-hud` consumer, not of
this interface.

## Bus topology

- **Bus**: session bus.
- **Well-known name**: `com.canonical.Myna.Dictation` (owned by `myna-desktop` when run with
  `--dbus`). Absence of an owner = daemon not running → the indicator stays
  dormant (FR-018).
- **Object path**: `/com/canonical/Myna/Dictation`.
- **Interface**: `com.canonical.Myna.Dictation`.

## Members

### Properties (read-only to the extension; every update pushed via `PropertiesChanged`)

| Property | Type | Range / values | Meaning |
|---|---|---|---|
| `State` | `s` | `idle`\|`loading`\|`recording`\|`transcribing`\|`finalizing`\|`notice`\|`error` (additive) | current dictation state (E1). **(2026-07-30)** `notice` is new: a recoverable, non-blocking issue (e.g. empty-transcript completion) — additive per §Compatibility, so an unpatched client degrades it to the existing neutral "active" treatment (FR-008). |
| `AudioRms` | `d` | `[0.0, 1.0]` | RMS level; `0.0` when idle (E2) |
| `AudioPeak` | `d` | `[0.0, 1.0]` | peak level; `0.0` when idle (E2) |
| `StatusMessage` | `s` | content-free publisher-owned label; `""` while `State==idle` | user-facing status label (E3): every visible state has one, including `loading`, `recording`, `transcribing`, and `finalizing`. |
| `HudStyle` | `s` | a `hud-style` settings nick (`bar`\|`ribbon`\|`vumeter`\|`progress`, additive); `""` from a publisher predating the property | **(2026-09-09)** which audio-level presentation the renderer should draw. The publisher is the only settings reader; the renderer is told, and resolves an unknown or empty nick to its default (C8). |
| `AudioDroppedNotActive` | `t` | count, cumulative per session | chunks refused because the session was already over. Only ever a bug. |
| `LastError` | `s` | content-free publisher-owned headline; `""` until the first failure | **(2026-10-01)** the headline of the latest failure, exactly as `StatusMessage` showed it (without the `Error: ` prefix). Sticky: set on every failure report, outside the `State`/`StatusMessage` dedup, and never cleared by the auto-hide or by a later success. Waiting at login for a portal that is not running yet is not a failure and leaves it alone. |
| `LastErrorDetail` | `s` | untranslated error text; `""` until the first failure | **(2026-10-01)** the cause behind `LastError` (e.g. `cannot reach the model: ...`). May name filesystem paths, commands or server-supplied text: consumers redact it before showing or exporting it. |
| `LastErrorTime` | `x` | microseconds since the Unix epoch; `0` for never | **(2026-10-01)** when `LastError` was set. Updated even when the headline repeats. |

The drop counter is a count, never samples or content, and exists because the
daemon is the only process that sees a chunk refused - the Settings app reads
it for its diagnostics page.

The three `LastError*` properties exist for the same reader: the pill is gone
by the time someone opens Settings Diagnostics, so the failure is kept beside
it. They are additive; a consumer talking to an older publisher that lacks
them reads "no last error", never a failure. Each is set with its own
`PropertiesChanged`, `LastErrorDetail` and `LastErrorTime` before
`LastError`, so a client reacting to `LastError` reads a consistent triple.

### Signals

None of its own. State transitions and level updates are pushed exclusively
with the standard `org.freedesktop.DBus.Properties.PropertiesChanged` — see
§Confinement for why the interface defines no custom signals. `StatusMessage`
is set *before* `State` on a transition, so a client reacting to the `State`
change already reads the consistent label.

### Methods

| Method | Signature | Effect |
|---|---|---|
| `Start` | `() → (b ok, s error)` | begin a session (equivalent to a hotkey Press); `ok=false` + reason if unavailable/blocked |
| `Stop` | `() → ()` | end the active session (graceful, like Release); no-op if idle |
| `Toggle` | `() → ()` | Start if idle, else Stop (the panel-button action, R8) |
| `RegisterClient` | `() → (u count)` | register the caller's unique bus name (`:1.xxx`) as a HUD client; idempotent, returns current client count. The server monitors `NameOwnerChanged` for the sender so a crashed client is pruned without an explicit `UnregisterClient` |
| `UnregisterClient` | `() → (u count)` | unregister the caller; idempotent, returns current client count |
| `PreviewSounds` | `(s set) → ()` | **(2026-10-05)** play the start, stop and error cues of the `sound-set` nick `set`, for Myna Settings' Preview button; returns once queued. Refuses with `com.canonical.Myna.Dictation.Error.Busy` while a session is under way (`State` other than `idle`, `notice`, `error`), `.AlreadyPlaying` while a preview plays, `.UnknownSoundSet` for an unknown nick, `.NoPlayer` when the daemon has no sound player |

## Guarantees (each a test row)

| # | Guarantee | Verified by |
|---|---|---|
| C1 | Owning `com.canonical.Myna.Dictation` on the session bus makes `State`/levels observable (read + `PropertiesChanged`) by a standard D-Bus client. | env-gated `dbus_hw.rs` round-trip |
| C2 | Every controller state transition publishes exactly one `State` property update with the mapped `State` string (E1 table), pushed via `PropertiesChanged`. | hermetic `dbus_indicator.rs` (fake bus) |
| C3 | `State`/`StatusMessage` never carry transcript text; `StatusMessage` is the content-free publisher-owned label for every visible state. **(2026-10-01)** Likewise `LastError`/`LastErrorDetail`: `LastError` is a fixed publisher-owned headline like `StatusMessage`; `LastErrorDetail` is error text that may carry paths, which consumers must redact before showing or exporting it. | hermetic assertion on published payloads |
| C4 | `AudioRms`/`AudioPeak` reflect the latest `AudioStats` while recording and are `0.0` at idle; updates are throttled to ~15–20 Hz. | hermetic (fake bus, fed AudioStats) + gated cadence check |
| C5 | The cold-load window publishes `loading` (not `recording`) until `Ready`; then `recording` (FR-006, R4). | hermetic mapping test |
| C6 | `Toggle` produces a Press edge when idle and a Stop when active; repeated/duplicate calls do not start two sessions (dedup, mirrors `ControlTrigger`). | hermetic `DbusTrigger` test |
| C7 | `Start` returns `(false, reason)` — not a panic — when a session cannot start (no target / secure field / backend down), with a content-free reason. | hermetic + gated |
| C8 | Unknown/extra `State` values are additive: a client that doesn't recognize one MUST NOT break (contract is unknown-tolerant). | documented + extension-side C-ext tests |
| C9 | With no owner, a client sees the name absent and no signals; when `myna-desktop` starts/stops, name-appeared/vanished fire. | env-gated + extension lifecycle test |
| C10 | **(2026-07-30)** A session that completes with an empty/blank transcript publishes `notice` (not `idle`), with a fixed content-free `StatusMessage`; a non-empty completion publishes `idle` exactly as before. | hermetic `dbus_indicator.rs` + `controller.rs` (empty vs. non-empty transcript cases) |
| C11 | **(2026-07-30)** The live per-event path (`event_to_indicator`'s `Done(_)` arm) and the finalize-block safety net (`SessionOutcome::Completed`) always agree on `notice` vs. `idle` for the same transcript — both route through one shared `completion_indicator_state()` helper, so they can never publish conflicting states, and a redundant second call is a no-op under C2's per-wire-state dedup. | hermetic `controller.rs` (asserts both call sites produce identical `IndicatorState` for the same transcript) |
| C14 | **(2026-08-28)** A `RegisterClient` call adds the sender's unique name to the client set (idempotent) and `UnregisterClient` removes it; the server also prunes vanished names via `NameOwnerChanged`. Return value is the current client count. | hermetic `serve.rs` client registry + gated round-trip |
| C15 | **(2026-08-28)** While at least one client is registered, the notification fallback is suppressed and the D-Bus HUD is the indicator; when the last client leaves (explicit `UnregisterClient` or vanished), the fallback is restored. The D-Bus `State` publishing is unaffected. | hermetic `dynamic.rs` + gated |
| C16 | **(2026-10-01)** Every failure report sets `LastError`/`LastErrorDetail`/`LastErrorTime`, including one whose headline repeats the previous one; the auto-hide back to `idle` and later sessions leave them untouched. | hermetic `indicator/dbus.rs` + gated `dbus_hw.rs` round-trip |
| C17 | **(2026-10-05)** `PreviewSounds` plays only between sessions and never stacks previews; each refusal carries its own error name. Additive: a client calling an older publisher gets `UnknownMethod`. | hermetic `serve.rs` + `sound/player.rs`, gated `dbus_hw.rs` round-trip |

## HUD identity: `com.canonical.Myna.Hud` (2026-08-28)

- **Well-known name**: `com.canonical.Myna.Hud` on the session bus, owned by the `myna-hud` `Adw.Application` singleton. Not watched directly for fallback — the HUD registers via `RegisterClient` on `com.canonical.Myna.Dictation` and the publisher's client set is authoritative.

## Presence: Client responsibility

Fallback suppression now uses the `RegisterClient` client set (C14/C15).

## Confinement (why properties only)

A strictly-confined snap publishing this interface (feature 005) lives under
snapd's `dbus` slot AppArmor policy, which grants a service **receive** from
unconfined peers (method calls + implicitly-allowed replies) and **send**
pinned to `peer=(name=org.freedesktop.DBus, label=unconfined)` on its own
path with the `org.freedesktop.DBus.Properties` interface. dbus-daemon
mediates a destination-less broadcast against the peer name
`org.freedesktop.DBus`, so that rule admits exactly one thing: broadcasting
**`PropertiesChanged` on the service's own path** to any subscriber,
confined or not. A custom-interface signal (e.g. a bespoke `StateChanged`)
has no matching rule and is AppArmor-denied for unconfined subscribers — and
AppArmor dbus rules cannot discriminate message types, so snapd cannot
safely widen this (a general send grant would also cover method calls to
unconfined peers; snapd deliberately removed that shape in 2016,
canonical/snapd#1613). Standard properties are therefore the *only*
confinement-proof push channel, and this interface uses nothing else.
Consumers must treat `State`/`StatusMessage`/levels as properties (read
current values, e.g. from a proxy cache) rather than relying on signal arg
bundling.

## Compatibility

Additive, matching the project's transport rule: new properties/signals/state
values may be added without a break; consumers ignore unknowns (C8). **(2026-07-30)**
The `notice` state value is exactly this kind of additive change — realized
entirely within the existing `State`/`StatusMessage` properties, no new members
were needed. No version token on the interface for the MVP (the state-string
additivity covers evolution); if a semantic break is ever needed it will be a
new interface name.

## Note: severity is interim, not a wire-level disposition (2026-07-30)

The `notice`/`error` split is a **client-inferred classification**
(`myna-desktop` computes it from an empty-transcript check, see
`data-model.md` E1a and `research.md` R13), not a true error disposition
carried end-to-end from the inference backend. It is a stopgap ahead of
T31/T62's proper error-taxonomy work; this contract does not attempt to model
that taxonomy, only the two coarse severities this feature needs.
