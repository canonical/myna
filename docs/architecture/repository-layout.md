# Repository and Module Layout

**Date:** 2026-07-19
**Status:** Accepted
**Authors:** Claude, with Charles

*Updated 2026-07-31 to document the Rust client, snap packaging, and the
platform-mandated GJS Shell extension added after the original record.*

## Context

Myna spans two deliverables that share one protocol: the candidate-adapter
evaluation testbed (Taipei lab, phases 0–4) and the Ubuntu Desktop dictation
client (UD129). Both speak the IE115-shaped session contract (WebSocket over
Unix socket, JSON events, PCM binary frames). The project is polyglot by
design: the server / testbed / adapters are Python; the production push-to-talk
client is Rust; inference is packaged as Ubuntu snaps; GNOME compositor UI is a
GJS extension. We need a layout that keeps the shared contract unambiguous across
all four build graphs without
forcing them to share a single package manager or language runtime.

## Decision

### Python side — `server/`

A single `uv`-managed Python project at `server/` (`server/pyproject.toml`,
`server/uv.lock`), providing an installed wheel named `myna`. Package source
lives at `server/src/myna/`:

```
myna.core     <- shared vocabulary; depends on nothing else in myna
myna.testbed  -> depends only on myna.core
```

- `myna.core` — the **load-bearing shared contract**. Audio types
  (`AudioFormat`, `PcmChunk`), the IE115 event vocabulary (`events.py`,
  `wire_ie115.py`), session config, protocol version, capabilities discovery,
  transport abstraction (`SttClient`/`SttSession`/`SttService` protocols, the
  in-process `LoopbackClient`, and the `WsUnixIe115Client` WebSocket transport).
  Used by `myna.server`, every `myna.testbed` adapter, and the full test suite.
  It is **not legacy and not to be collapsed** — it is the canonical Python
  expression of the wire contract.
- `myna.server` — the `myna-server` process that the inference snaps ship.
  Wraps the adapters behind the IE115 WebSocket transport.
- `myna.testbed` — `Candidate`/`Adapter`, the permanent `FakeAdapter` regression
  fixture, audio sources (WAV, live-mic), the `Harness` with its
  `ResultRecord`/metrics schema, bench driver and corpus tooling.

The Ubuntu Desktop dictation client (UD129 — session controller, IBus text
injection, activity indicator) is **not** a Python package: it lives in the Rust
`client/myna-desktop` crate (feature 003-desktop-injection). The former
`myna.desktop` interface stubs were retired once that contract landed in Rust;
see `../desktop-injection.md`.

Interfaces are `typing.Protocol`s, not ABCs: adapters and backends are
structural plug-ins and should not need to import a base class to conform.

Model/engine dependencies (faster-whisper, NeMo, OpenBLAS/ctypes) are `uv`
optional-dependency extras scoped per adapter (`[whisper]`, `[nemotron]`),
never imported by `myna.core` or the harness.

### Rust side — `client/`

A Cargo workspace with five crates:

```
client/
  myna-core/          <- wire contract mirror (IE115 types, FSM events)
  myna-audio/         <- capture adapter (pipewire-rs backend; AudioSource /
  |                      CaptureBackend traits)
  myna-orchestrator/  <- session/residency FSM (wire-agnostic)
  myna-cli/           <- myna-dictate binary (testbed demo: WAV/corpus/mic)
  myna-desktop/       <- myna-desktop app (hotkey + IBus injection + indicator)
```

- `myna-core` — **mirrors** the Python `myna.core` wire contract for the Rust
  client: IE115 event types, session config shapes, the wire codec. It is the
  Rust peer of `myna.core`, not a duplicate to collapse. The two live in
  different processes and different language runtimes; they are kept consistent
  by the shared IE115 spec, not by shared code.
- `myna-audio` — live microphone capture via a native `pipewire-rs` backend
  (the sole live-capture path since T52; the `pw-record` subprocess was
  retired). Exposes `AudioSource`/`CaptureBackend` traits so the FSM is
  transport-agnostic.
- `myna-orchestrator` — push-to-talk session / residency FSM. Drives model
  loading, the `preparing` → `ready` → `transcribing` lifecycle, and
  multi-commit IE115 sessions. Wire-agnostic: speaks `myna-core` types.
- `myna-cli` — the `myna-dictate` binary: the testbed/demo push-to-talk client
  (WAV clip / corpus / live mic, stdin hotkey stand-in), wiring `myna-audio` +
  `myna-orchestrator` + the IE115 WebSocket transport together.
- `myna-desktop` — the shipped `myna-desktop` dictation app (feature
  003-desktop-injection, T21/T22): the `DesktopController` composing a
  GlobalShortcuts-portal hotkey (`Trigger`), an IBus-over-`zbus` text injector
  (`Injector`), and a GTK4 activity indicator (`Indicator`) over the same
  `myna-orchestrator` session. Each boundary is a trait with a mock; see
  `../desktop-injection.md`.

### GNOME Shell side — `extensions/myna-shell/`

A platform-mandated GJS extension loaded inside GNOME Shell. Feature 004 defines
the `org.myna.Dictation` publisher/consumer boundary; feature 009 defines the
current presentation: Basic energy bar by default, Wave ribbon selectable via a
local GSettings preference. The extension is pure UI and never captures audio,
transcribes, or injects text. Operational documentation lives in
`../../extensions/myna-shell/README.md`.

### Snap packages — `whisper-snap/`, `nemotron-snap/`, `qwen-snap/`, `parakeet-snap/`, `sherpa-snap/`

One snap per model family. Each snap:
- Bundles model weights as snap components.
- Builds the `myna` Python wheel (with the family's extras) from `server/src/myna/`.
- Runs `myna-server` as the confined IE115 endpoint.
- Supports idle-unload (modelctl/IE108) and model lifecycle management.

The snap build is the canonical production deployment of `myna.server`; the
`uv`-managed dev install is for testbed and development only.

## Rationale

### Why polyglot?

The testbed, server, and adapters need fast iteration on model-specific logic
(NeMo, CTranslate2, OpenBLAS/ctypes) in Python — the ML ecosystem lives there.
The production push-to-talk client needs native PipeWire access, low latency,
and a real-time FSM that does not block on the Python GIL or import the ML
stack — Rust is the right tool. The snap packaging boundary keeps the two
runtimes cleanly separated.

### Why two `core` crates?

The Python `myna.core` and Rust `myna-core` are **peer mirrors of one
contract**, not accidental duplicates. They ship in different processes
(server vs client), different language runtimes, and must be independent of
each other's build graph. Collapsing them would require either (a) FFI
complexity that buys nothing, or (b) giving up the language-idiomatic type
systems that make each side safe. The canonical source of truth is the IE115
spec; each side's `core` is its typed expression of that spec.

### Python layout alternatives considered

- **`uv` workspace with separate packages per deliverable** — right shape if
  the testbed ships independently, but premature while interfaces churn; every
  cross-package change would need lockstep version bumps. The subpackage
  boundaries preserve the option.
- **Separate repos for testbed and desktop** — maximises drift risk on exactly
  the thing that must not drift (the event/session contract), for no benefit
  at this stage.
- **ABC base classes instead of Protocols** — forces adapters to depend on
  `myna` at import time; Protocols keep candidates' messy environments (vLLM,
  NeMo) decoupled and make conformance checkable structurally.

### Rust layout alternatives considered

- **Single `myna` crate** — simpler, but mixes the wire contract (stable, used
  everywhere) with the capture backend (hardware-specific, frequently changing)
  and the FSM (logic-heavy). The four-crate split makes each crate's dependency
  set and test surface clear.
- **`myna-core` as a sub-crate of the Python package** — impossible without FFI
  and would create a circular build dependency between the snap (which builds
  the Python wheel) and the Rust binary.

## Consequences

- The harness can never grow a model-specific code path: it only sees
  `myna.core` types. Candidate messiness is forced into adapters by the import
  graph, not just by convention.
- The desktop client and testbed cannot drift apart on event semantics — there
  is one `events.py` on the Python side; `myna-core` tracks it on the Rust side.
- The snap and GJS build graphs are independent of the Rust client build; adding a new
  model family snap requires only a new `NNN-snap/` directory without touching
  the Rust workspace.
- Layout changes to the directory tree (renaming `client/`, renaming
  crates, restructuring `server/src/myna/`) should be proposed by updating **this ADR**
  first, so the rationale travels with the change. Use `git mv` to preserve
  blame history; do not rewrite.
- If the testbed later needs to be deployable standalone (Taipei lab), the
  Python package can be split into a `uv` workspace along the existing
  subpackage boundaries without rework.
