# Contract: `org.myna.Dictation` (session bus)

**Feature**: 004-gnome-shell-indicator | **Date**: 2026-07-21

The single seam between `myna-desktop` (publisher, Rust/`zbus`) and the GNOME
Shell extension (consumer, GJS/`Gio.DBusProxy`). State + level only — never
transcript content (constitution V). This contract is defined here and encoded as
executable tests on both sides before implementation.

## Bus topology

- **Bus**: session bus.
- **Well-known name**: `org.myna.Dictation` (owned by `myna-desktop` when run with
  `--dbus`). Absence of an owner = daemon not running → extension stays dormant
  (FR-018).
- **Object path**: `/org/myna/Dictation`.
- **Interface**: `org.myna.Dictation`.

## Members

### Properties (read-only to the extension; every update pushed via `PropertiesChanged`)

| Property | Type | Range / values | Meaning |
|---|---|---|---|
| `State` | `s` | `idle`\|`loading`\|`recording`\|`transcribing`\|`finalizing`\|`error` (additive) | current dictation state (E1) |
| `AudioRms` | `d` | `[0.0, 1.0]` | RMS level; `0.0` when idle (E2) |
| `AudioPeak` | `d` | `[0.0, 1.0]` | peak level; `0.0` when idle (E2) |
| `ErrorMessage` | `s` | content-free reason; `""` unless `State==error` | user-facing error reason (E3) |

### Signals

None of its own. State transitions and level updates are pushed exclusively
with the standard `org.freedesktop.DBus.Properties.PropertiesChanged` — see
§Confinement for why the interface defines no custom signals. `ErrorMessage`
is set *before* `State` on a transition, so a client reacting to the `State`
change already reads the consistent reason.

### Methods

| Method | Signature | Effect |
|---|---|---|
| `Start` | `() → (b ok, s error)` | begin a session (equivalent to a hotkey Press); `ok=false` + reason if unavailable/blocked |
| `Stop` | `() → ()` | end the active session (graceful, like Release); no-op if idle |
| `Toggle` | `() → ()` | Start if idle, else Stop (the panel-button action, R8) |

## Guarantees (each a test row)

| # | Guarantee | Verified by |
|---|---|---|
| C1 | Owning `org.myna.Dictation` on the session bus makes `State`/levels observable (read + `PropertiesChanged`) by a standard D-Bus client. | env-gated `dbus_hw.rs` round-trip |
| C2 | Every controller state transition publishes exactly one `State` property update with the mapped `State` string (E1 table), pushed via `PropertiesChanged`. | hermetic `dbus_indicator.rs` (fake bus) |
| C3 | `State`/`ErrorMessage` never carry transcript text; `ErrorMessage` is a content-free reason. | hermetic assertion on published payloads |
| C4 | `AudioRms`/`AudioPeak` reflect the latest `AudioStats` while recording and are `0.0` at idle; updates are throttled to ~15–20 Hz. | hermetic (fake bus, fed AudioStats) + gated cadence check |
| C5 | The cold-load window publishes `loading` (not `recording`) until `Ready`; then `recording` (FR-006, R4). | hermetic mapping test |
| C6 | `Toggle` produces a Press edge when idle and a Stop when active; repeated/duplicate calls do not start two sessions (dedup, mirrors `ControlTrigger`). | hermetic `DbusTrigger` test |
| C7 | `Start` returns `(false, reason)` — not a panic — when a session cannot start (no target / secure field / backend down), with a content-free reason. | hermetic + gated |
| C8 | Unknown/extra `State` values are additive: a client that doesn't recognize one MUST NOT break (contract is unknown-tolerant). | documented + extension-side C-ext tests |
| C9 | With no owner, a client sees the name absent and no signals; when `myna-desktop` starts/stops, name-appeared/vanished fire. | env-gated + extension lifecycle test |

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
Consumers must treat `State`/`ErrorMessage`/levels as properties (read
current values, e.g. from a proxy cache) rather than relying on signal arg
bundling.

## Compatibility

Additive, matching the project's transport rule: new properties/signals/state
values may be added without a break; consumers ignore unknowns (C8). No version
token on the interface for the MVP (the state-string additivity covers evolution);
if a semantic break is ever needed it will be a new interface name.
