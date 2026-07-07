# CLAUDE.md — UbuSTT (myna) project context

Lean context for a coding agent. **`docs/project-plan.md` is the living task
tracker (stable IDs T01–T32) — read it first** for status and next steps.

## What this is

Ubuntu Desktop speech-to-text (dictation): hold a hotkey, speak, transcribed
text is inserted into the focused app. Local, offline, privacy-preserving — no
cloud, no persistent audio.

## Current state (2026-07-01)

- **Testbed**: harness + session contract over two transports (loopback,
  WebSocket-UDS), same contract tests; fake adapter (permanent fixture); WAV +
  live-mic sources; WER/CER metrics; capabilities discovery (T24); bench + matrix
  aggregator (`dev/bench.py`, `dev/aggregate.py`). Two corpora, both regenerated
  (gitignored), not committed: synthetic espeak (`fixtures/`,
  `dev/generate_fixtures.py`) and **real recorded speech** (`corpus/real/`,
  LibriSpeech CC-BY, `dev/fetch_real_corpus.py` — T25). Real WER is trustworthy;
  synthetic WER is plumbing/latency only (Nemotron: 0% real vs 44.6% synthetic,
  same model).
- **Adapters** (built, hardware-verified): faster-whisper (AED), Nemotron /
  FastConformer (native transducer), and **Qwen3-ASR** via a verified
  pure-C/ctypes adapter (`qwen-c`, zero pip deps, multilingual CPU, shipped).
  A vLLM/GPU runtime for Qwen3 lives on the `qwen3-vllm-gpu` branch (parked).
  `myna-server --adapter whisper|nemotron|qwen-c` serves any on a UDS.
- **Snaps**: `whisper-snap/`, `nemotron-snap/`, `qwen-snap/` (one per family) —
  modelctl/IE108, weights as components, GPU engines, idle-unload; run in strict
  confinement. `qwen-snap` ships the pure-C CPU engine; a GPU engine for the
  family is on the `qwen3-vllm-gpu` branch (showing runtimes are switchable per
  family via the existing engine mechanism).
- **Desktop**: `dev/dictate.py` push-to-talk demo. Session controller + IBus
  injection (T21/T22) not started.
- **Spec (IE115)**: Workstream F **resolved (2026-07-01)** — the team settled on
  IE115 as a *suitable subset* of OpenAI's Realtime API + additive events
  (compat / remote-backend / industry-contribution). Adopted: our model-loading
  **liveness event** (loading is a lifecycle state, not an error) and
  **capabilities discovery as a separate models API**. Overruled for compat:
  flat-profile + drop-`conversation.item`. Out of scope: translation. Full
  mapping in `docs/IE115-resolution.md`; async lifecycle diagrams in
  `docs/architecture/ie115-lifecycle.md`. Still open: error taxonomy (T31),
  protocol versioning (T35), overload/lag signal, GPU memory pressure.
- **Next**: **audio adapter** (Charles — took over workstream D, 2026-07-07):
  the `rust/myna-audio` crate behind the `AudioSource` trait (now in
  `myna-core::capture`) — `CaptureBackend` seam, adapter-owned pre-ready ring
  (drop-oldest + stats tap; real DSP stays in PipeWire filter-chain). T49 (API
  v2, `docs/audio-adapter-api.md`) and T50 (skeleton crate + `ScriptedBackend`
  fake, drop-in proven through the T41 runner) are **done**; T51 (pw-record
  subprocess backend, `myna-dictate --mic`) is code-complete and live-verified
  against the fake adapter — one spoken whisper run remains, then T52
  (native pipewire-rs). Inference snap server: Ivano.

## Invariants (don't violate)

- **Audio-push**: the *client* owns PipeWire capture and pushes PCM; the STT
  service has no microphone access. Design interfaces accordingly. The client
  also owns format conversion: the service advertises accepted `input_formats`
  (capabilities, T24) and **rejects** off-format audio — adapters never resample.
- **Never persist audio; don't log transcription content by default.**
- **Fix the adapter, not the harness.** The harness speaks only the IE114-shaped
  `myna.core` interfaces; all model messiness lives in adapters. The fake adapter
  is a permanent regression fixture.
- **Transport behind an abstraction.** WebSocket-over-UDS is implemented
  (`myna/core/transport_ws.py`; snaps serve `ws+unix`) — keep
  adapters/harness transport-agnostic.

## Transport & events

WebSocket over a Unix socket: PCM binary frames in, JSON events out, one
connection per session.

The **server speaks first** (2026-07-06, hazard review): on connect it sends
one `session.created` greeting carrying the served `protocol_version`
(`myna.core.protocol`, T35) *and* the IE115 session defaults — so a stock
OpenAI client (which waits for `session.created`) can't deadlock against the
shape-sniff, and version-aware clients on either dialect learn the version.
The internal client then declares `protocol_version` in `session.start`; on
mismatch the server sends a terminal
`transcription.error(unsupported_protocol_version)`. The version is in-band
(not a WS subprotocol token) so it stays transport-agnostic, and it versions
the whole contract — event vocab + config + capabilities shapes — as one
number. Compat is **additive** (aligned with IE115, 2026-07-06): unknown event
types / frames / phases are ignored on both ends, so adding an event is NOT a
bump; bump only for semantic changes to existing events/shapes. The IE115
dialect itself is declared unversioned-additive. Control frames carry a `type`
key; transcript events carry an `event` key.

Event vocabulary (`myna.core.events`) — this is myna's **internal** contract.
The team's wire direction is now IE115 (OpenAI-subset) event names
(`session.*`, `input_audio_buffer.*`, `conversation.item.input_audio_transcription.*`)
plus additive events (the liveness/`STATUS` event). This is now **implemented on
both ends as a selectable wire dialect** (T43/T45/T46): `myna.core.wire_ie115`
(Python codec) + shape-sniff dispatch in `transport_ws` (`WsUnixIe115Client`);
`WsUnixIe115Backend` in Rust (`myna-dictate --dialect ie115 [--base64-audio]`).
The internal flat vocab stays the semantic core; the codec translates on each
end, so the Rust FSM is untouched. Verified across whisper/nemotron/qwen-c.
IE115 connections are **persistent** (T47, 2026-07-06): multi-commit per
connection, `final`↔`delta` / `done`↔`completed` (per-utterance `item_id`), the
*client* closes after its commit's `completed`; close-before-`completed` is a
`connection_closed` error, never a synthesised done. (Internal dialect stays
one-shot; recorded intent is for it to retreat to loopback/testbed-only once
IE115 is the team wire — see plan T48.) A `session.…transcription.model` the
server doesn't serve is **rejected** (`model_not_available`), never silently
substituted; which socket serves which model is T48 (backend discovery,
unowned). Every `transcription.error` is terminal on the wire today — T31 must
put any recoverable/advisory disposition **on the wire**, not in client code
tables. See `docs/architecture/ie115-wire.md` (frame contract) and
`docs/IE115-resolution.md`.
Internal vocab:
- `transcription.progress` — liveness; `phase` is `preparing` (model loading),
  `ready` (model resident, gate open, nothing decoding yet — client may send
  audio), or `transcribing`. Optional unstable `snippet` for UI; never committed
  text. The three phases map onto the IE115 `STATUS` liveness `state`.
- `transcription.final` — committed text for a segment; never retracted.
- `transcription.done` — terminal; full transcript.
- `transcription.error` — terminal; `code` + `message`.

No `partial`/`replace`/epoch retraction (dropped as confusing). Adding an event?
Document it here and flag it provisional (additive: old clients ignore it — no
version bump needed).

Discovery: before a session a client may send `capabilities.query` (WS) /
`client.capabilities()` and get a `Capabilities` doc (models, languages,
`input_formats`, punctuation, translation) — `myna.core.capabilities`, T24,
provisional.

## Models

| Model | License | Notes |
|---|---|---|
| Whisper (faster-whisper) | MIT | AED; streaming is bolt-on chunked re-decode (LocalAgreement). CTranslate2. Built + snapped. |
| Nemotron / FastConformer | — | Cache-aware RNNT, *natively streaming* (each frame once), `att_context_size` latency dial, native punctuation, English-only. NeMo. Built + snapped. |
| Qwen3-ASR | Apache-2.0 | Multilingual (30 langs), LLM decoder, prompt biasing. Shipped via pure-C/OpenBLAS through ctypes (CPU, zero pip deps, verified). A vLLM/GPU runtime is on the `qwen3-vllm-gpu` branch (parked). |

Key distinction: native transducer (Nemotron) vs AED re-decode (Whisper) drives
streaming latency / partial churn. The Open ASR Leaderboard (batch WER) can't
answer dictation-quality questions — the testbed exists to fill that gap.

Model cache: `HF_HOME` fixed dir; `hf download` (resumable); verify offline with
`HF_HUB_OFFLINE=1`.

## Environment & conventions

- Tooling `uv`; GPU CUDA; PipeWire audio. Extras: `whisper`, `nemotron`.
- New spec artifacts: plain text, IE114/UD129 house style. Design notes in
  `docs/asr-inference-snap-design.md` + `docs/architecture/`.

## Open questions (plan workstream E)

Error model / stable codes (T31 — requirement recorded: disposition rides the wire);
performance contract / latency SLOs (needs T12 lab runs); residency default policy
(T29 — decided **together with** the client capture-buffer depth, see audio-adapter-api §6);
backend discovery / model selection across snaps (T48, unowned); audio sample-encoding
in `input_formats` (T33, **team discussion** — position recorded in the plan row:
keep s16le, add an `encoding` discriminant, int16→float32 is decode-side reinterpretation).
