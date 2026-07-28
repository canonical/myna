# CLAUDE.md — UbuSTT (myna) project context

Lean context for a coding agent. **`docs/project-plan.md` is the living task
tracker (stable global IDs, currently T01–T56) — read it first** for status and
next steps.

## Working on this repo (spec-kit)

Spec-kit (speckit) was adopted **mid-development** (2026-07-15), so the repo has
a "before" and "after": early work landed ad-hoc against `docs/project-plan.md`;
features **002 (native PipeWire backend)**, **003 (desktop injection)**, and
**004 (GNOME Shell indicator)** were built through the spec-driven flow under
`specs/NNN-*/`, governed by `.specify/memory/constitution.md` (v1.3.0). **New
work should use the spec-kit workflow** — see the `speckit-*` skills
(`specify` → `plan` → `tasks` → `implement`, plus `clarify`/`analyze`/`checklist`).

Two task-numbering schemes exist and do **not** correspond: the plan's global
`TNN` IDs, and the per-feature `T0NN` numbers inside each `specs/NNN-*/tasks.md`.
E.g. plan **T52** (native PipeWire backend) shipped as feature
`002-native-pipewire-backend`, whose own tasks run T001–T038. When a task ID is
ambiguous, say which scheme.

The constitution binds shipped Rust components (TDD, integration-readiness,
performance watermarks, Workshop dev env, privacy/offline). The Python
testbed/`myna-server` and the GJS extension are **evaluation-harness tier** —
exempt from strict TDD / watermarks / the Rust-language rule, but still bound by
the privacy + offline invariants.

## What this is

Ubuntu Desktop speech-to-text (dictation): activate a hotkey, speak, transcribed
text is inserted into the focused app. Local, offline, privacy-preserving — no
cloud, no persistent audio.

## Code map

- `server/src/myna/` — the **Python** side. `myna.core` is the shared session
  contract (audio / events / session / capabilities / protocol / transports,
  incl. the IE115 wire codec). **Load-bearing, not legacy**: `myna.server` (the
  `myna-server` process the snaps ship), *every* `myna.testbed` adapter
  (whisper / nemotron / qwen / fake / harness / sources), and the test suite all
  import it.
- `client/` — the **Rust** dictation client. `myna-core` mirrors the wire
  contract (+ capture consumer traits); `myna-audio` is the capture adapter;
  `myna-orchestrator` the session/residency FSM; `myna-cli` the `myna-dictate`
  testbed binary; `myna-desktop` the shipped push-to-talk app (hotkey + IBus
  injection + opt-in IBus preedit partials via `--preedit` + activity indicator
  + the `org.myna.Dictation` D-Bus publisher).
  The production hot path.
- `extensions/myna-shell/` — the **GJS** GNOME Shell extension (feature 004): a
  focus-safe in-compositor dictation indicator ("goop"/ribbon) that consumes
  `org.myna.Dictation` from `myna-desktop`. Pure UI — never captures, transcribes,
  or injects. Non-Rust by platform necessity (in-compositor UI must be GJS).
- `myna-snap/` — the **client snap** (feature 005): packages `myna-desktop` +
  `myna-dictate` as the strictly-confined `myna` snap (no `network` plug).
  Reaches a backend snap's session socket over the `ubustt-socket` writable
  content share (T14c); `dev/prepare.sh` stages `client/` before packing.
- `whisper-snap/`, `nemotron-snap/`, `qwen-snap/` — one inference snap per model
  family; strict confinement.
- **Two `core`s on purpose.** Python `myna.core` (server + testbed) and Rust
  `myna-core` (client) are peer mirrors of one contract shipping in different
  processes/languages — not duplicates to collapse.

## Current state

- **Testbed**: harness + session contract over two transports (loopback,
  WebSocket-UDS); fake adapter (permanent fixture); WAV + live-mic sources;
  WER/CER metrics; capabilities discovery; bench + matrix aggregator
  (`dev/bench.py`, `dev/matrix.py`, `dev/aggregate.py`). Two corpora, both
  regenerated (gitignored): synthetic espeak (`dev/generate_fixtures.py`) and
  **real recorded speech** (`corpus/real/`, LibriSpeech, `dev/fetch_real_corpus.py`).
  Real WER is trustworthy; synthetic WER is plumbing/latency only (Nemotron: 0%
  real vs 44.6% synthetic, same model).
- **Adapters** (built, hardware-verified): faster-whisper (AED), Nemotron /
  FastConformer (native transducer), and Qwen3-ASR via a pure-C/ctypes adapter
  (`qwen-c`, zero pip deps, multilingual CPU). `myna-server --adapter
  whisper|nemotron|qwen-c|fake` serves any on a UDS. A Qwen3 vLLM/GPU runtime is
  parked on `qwen3-vllm-gpu`.
- **Client (Rust)**: `myna-dictate` is the testbed/demo push-to-talk client —
  the wire-agnostic session/residency FSM over `myna-audio`'s native
  `pipewire-rs` capture (`--mic`, node selection by stable `node.name`,
  channel pick/downmix, `--list-devices`), speaking both the internal wire and
  IE115. `myna-desktop` is the shipped last-mile: a `DesktopController` composing
  a hotkey (GNOME shortcut / GlobalShortcuts portal), an IBus-over-`zbus` text
  injector (commit-only, focus/secure-field safe), a GTK4 activity indicator, and
  a `--dbus` mode serving `org.myna.Dictation` for the GNOME extension. See
  `docs/desktop-injection.md`.
- **GNOME extension** (feature 004): the focus-safe indicator on GNOME/Wayland,
  where a normal client can't show an always-on-top overlay. `myna-desktop --dbus`
  publishes state + audio level over `org.myna.Dictation`; the extension renders
  it (Cairo VU ribbon + content-free status label) behind a swappable
  `IndicatorView` seam. Contract in `specs/004-gnome-shell-indicator/`.
- **Snaps**: one per family — modelctl, weights as components, GPU engines,
  idle-unload, strict confinement.
- **Client snap (feature 005)**: the orchestrator ships as the confined `myna`
  snap (`myna-snap/`); backend socket via the `ubustt-socket` content share
  (T14c resolved for confined clients — identity/polkit stays T17). Verified
  confined end-to-end against the whisper snap; literal hotkey press + spoken
  injection are human acceptance. CI: `.github/workflows/snap.yml` builds and
  smoke-tests the snap. Packaging gotchas are recorded in the snapcraft.yaml
  comments (no `gnome` extension for Rust builds; rust-plugin/rustup-1.29
  workaround; the confined PipeWire staging set + XDG_RUNTIME_DIR symlink).
- **Streaming mode (feature 007, landed 2026-07-27)**: dual-mode streaming is
  implemented end-to-end — `disposition: committed|unstable` on IE115 deltas +
  `session.streaming` greeting field (contract:
  `specs/007-streaming-mode/contracts/streaming-wire.md`); `--streaming` on
  `myna-server` (whisper: commit-on-finalize + per-segment deltas; nemotron:
  sentence-split stand-in for the native loop); client FSM routes committed →
  `Final` (inject-safe) / unstable → `Unstable` (display-only, FR-007);
  `myna-dictate --mode auto|streaming|batch [--show-unstable]`; RTF tier gate
  via `results/streaming-tiers.json` + watermarks in
  `results/streaming-watermarks.json` (SC-002: streaming WER == batch WER;
  SC-004: commit-stability 100%). **Feature 008 (in flight,
  `specs/008-progressive-emission/`)**: the whisper adapter now streams for
  real — rolling re-decode loop (`server/src/myna/testbed/streaming/`) with
  three wire-invisible commit strategies (`--strategy local-agreement`
  default / `tail-mutation` / `fixed-head`); unstable ~1.5 s in, first commit
  ~2.5 s in on whisper-tiny CPU. FR-008 closed for whisper. Watermarks on
  26–28 s concatenated streams (`corpus/real/manifest-streams.json`):
  fixed-head == batch WER, LA/TM +2.4 pp (AED right-context cost; beam ruled
  out — part of that gap was commit-boundary dedupe bugs fixed 2026-07-28:
  the overlap alignment is now frontier-anchored at **character** level
  (squashed words), so re-decode churn — earlier-word edits, silence
  compression re-timing, merged/split tokens ("es"+"Carlos." → "escarlos.")
  — can't double-commit boundary words; watermarks want re-measuring). Spike S1 GO (0.997/0.982 agreement, tiny/base). Remaining: nemotron
  native loop (Spike S2, GPU), Parakeet + sherpa-onnx snaps, report.
  Interop report delivered: `docs/interop/canonical-whisper-snap-report.md` —
  the canonical/whisper-snap's deltas restate the growing hypothesis with no
  disposition field (verified live; `myna-cli/tests/interop_canonical.rs`).
  Settings: `docs/streaming-mode-settings.md`.
- **Open / next**: nemotron native transducer loop (008 US2, Spike S2 on the
  NVIDIA PC), Parakeet + sherpa-onnx small snaps (008 US3/US4), error taxonomy
  (T31, disposition must ride the wire), backend discovery / model selection
  across snaps (T48),
  toolchain fully under Workshop (T55), extension screen-reader announcements
  (T56). UD136 desktop-UX follow-ups: T58–T62. Inference snap server: Ivano.

## Invariants (don't violate)

- **Audio-push**: the *client* owns PipeWire capture and pushes PCM; the STT
  service has no microphone access. The client also owns format conversion: the
  service advertises accepted `input_formats` (capabilities) and **rejects**
  off-format audio — adapters never resample.
- **Never persist audio; don't log transcription content by default.** The
  `org.myna.Dictation` bus and the indicator carry state + normalized level only.
- **Fix the adapter, not the harness.** The harness speaks only the
  `myna.core` interfaces; all model messiness lives in adapters. The fake adapter
  is a permanent regression fixture.
- **Transport behind an abstraction.** WebSocket-over-UDS is the transport
  (`myna/core/transport_ws.py`; snaps serve `ws+unix`) — keep adapters/harness
  transport-agnostic.

## Transport & events

WebSocket over a Unix socket: PCM binary frames in, JSON events out, one
connection per session.

The **server speaks first**: on connect it sends a `session.created` greeting
carrying the served `protocol_version` (`myna.core.protocol`) and IE115 session
defaults — so a stock OpenAI client (which waits for `session.created`) can't
deadlock against the shape-sniff, and version-aware clients learn the version
in-band (not a WS subprotocol token, so it stays transport-agnostic). The version
covers the whole contract as one number. Compat is **additive**: unknown event
types / frames / phases are ignored on both ends, so adding an event is NOT a
bump — bump only for semantic changes to existing events/shapes. Control frames
carry a `type` key; transcript events carry an `event` key.

**Two selectable wire dialects**, both implemented end-to-end (verified across
whisper/nemotron/qwen-c):
- **Internal** flat vocab (`myna.core.events`) — the semantic core.
- **IE115** (OpenAI-Realtime-subset) event names (`session.*`,
  `input_audio_buffer.*`, `conversation.item.input_audio_transcription.*`) plus
  additive events (the liveness/`STATUS` event). `myna.core.wire_ie115` (Python
  codec) + shape-sniff dispatch in `transport_ws`; `WsUnixIe115Backend` in Rust
  (`myna-dictate --dialect ie115 [--base64-audio]`).

The codec translates at each edge, so the FSM never changes when the dialect
does. IE115 connections are **persistent** (multi-commit per connection,
`final`↔`delta` / `done`↔`completed` with per-utterance `item_id`); the *client*
closes after its commit's `completed`, and close-before-`completed` is a
`connection_closed` error, never a synthesised done. A requested model the server
doesn't serve is **rejected** (`model_not_available`), never silently
substituted. Every `transcription.error` is terminal on the wire today — T31 must
put any recoverable/advisory disposition **on the wire**, not in client tables.
See `docs/architecture/ie115-wire.md` and `docs/IE115-resolution.md`.

Internal vocab:
- `transcription.progress` — liveness; `phase` is `preparing` (model loading),
  `ready` (resident, gate open, nothing decoding yet — client may send audio), or
  `transcribing`. Optional unstable `snippet` for UI; never committed text. Maps
  onto the IE115 `STATUS` liveness `state`.
- `transcription.final` — committed text for a segment; never retracted.
- `transcription.done` — terminal; full transcript.
- `transcription.error` — terminal; `code` + `message`.

No `partial`/`replace`/epoch retraction on the wire *today* — but UD136
(2026-07-26) made a **streaming mode** a product requirement alongside batch:
the design-review direction is committed chunks shown progressively (append-only
holds); whether any *unstable* hypothesis text gets wire representation is an
open question for the streaming spec (T08/T63). Adding an event?
Document it here and flag it provisional (additive: old clients ignore it).

Discovery: before a session a client may send `capabilities.query` and get a
`Capabilities` doc (models, languages, `input_formats`, punctuation, translation)
— `myna.core.capabilities`, provisional.

## Models

| Model | License | Notes |
|---|---|---|
| Whisper (faster-whisper) | MIT | AED; streaming is bolt-on chunked re-decode (LocalAgreement). CTranslate2. |
| Nemotron / FastConformer | — | Cache-aware RNNT, *natively streaming* (each frame once), `att_context_size` latency dial, native punctuation, English-only. NeMo. |
| Qwen3-ASR | Apache-2.0 | Multilingual (30 langs), LLM decoder, prompt biasing. Shipped via pure-C/OpenBLAS through ctypes (CPU, zero pip deps). GPU runtime parked on `qwen3-vllm-gpu`. |

Key distinction: native transducer (Nemotron) vs AED re-decode (Whisper) drives
streaming latency / partial churn. The Open ASR Leaderboard (batch WER) can't
answer dictation-quality questions — the testbed exists to fill that gap.

Model cache: `HF_HOME` fixed dir; `hf download` (resumable); verify offline with
`HF_HUB_OFFLINE=1`.

## Environment & conventions

- Tooling `uv`; GPU CUDA; PipeWire audio. Python extras: `whisper`, `nemotron`.
  Canonical dev env is Canonical **Workshop** (`.workshop/myna.yaml`).
- New spec artifacts go under `specs/NNN-*/` via the spec-kit flow. Design notes
  in `docs/asr-inference-snap-design.md` + `docs/architecture/`.

## Open questions (plan workstream E)

Error model / stable codes (T31 — disposition rides the wire); performance
contract / latency SLOs (needs lab runs); residency default policy (T29, decided
together with the client capture-buffer depth); backend discovery / model
selection across snaps (T48); audio sample-encoding in `input_formats` (T33 —
keep s16le, add an `encoding` discriminant).
