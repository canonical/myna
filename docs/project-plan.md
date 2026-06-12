# Myna Project Plan

**Date:** 2026-06-12
**Status:** Living document — update task status in place as work lands.

This plan turns IE114 (UbuSTT API), UD129 (Desktop STT Integration), and the
testbed phasing from CLAUDE.md into assignable tasks. Task IDs are stable;
reference them in branches and PRs (e.g. `t02-fake-adapter`).

Open spec questions (transport, event vocabulary, capabilities discovery,
error model, performance contract) are tracked as tasks in workstream E so
they get owners instead of lingering.

## Workstreams

- **A — Testbed** (phases 0–4): produce the evaluation matrix and
  reference-hardware tiers.
- **B — Inference snap**: an IE114-conformant ASR inference snap, modelled on
  the qwen3/gemma4 snaps in `reference/`.
- **C — Transport**: the real IE114 wire protocol, behind the existing
  abstraction.
- **D — Desktop client**: the UD129 dictation experience.
- **E — Spec work**: feed findings back into IE114/UD129.

Dependencies at a glance: A is unblocked now and feeds E; C unblocks B and D's
integration work; B and D converge in the end-to-end milestone (T22).

## Status legend

`todo` · `in progress` · `done` · `blocked(<task>)`

---

## Workstream A — Candidate-adapter testbed

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T01 | Scaffolding: package layout, core types, transport abstraction | done | Claude/Charles | — | `myna.core` events/audio/session/transport; ADRs written |
| T02 | Fake adapter + harness skeleton (Phase 0) | done | Claude/Charles | T01 | `uv run python -m myna.testbed` produces a ResultRecord; contract tests in `tests/test_contract.py` pass |
| T03 | Test-fixture audio corpus: short utterances with reference transcripts, per UD129 accuracy matrix (quiet/noise, accents, commands, long-form, technical vocab) | done (synthetic tier) | Claude/Charles | — | `dev/generate_fixtures.py` synthesizes all matrix categories via espeak-ng (CC0, offline, deterministic noise variants); manifest schema + loader in `myna.testbed.corpus`. Real recorded speech is T25 |
| T04 | WAV/file audio source for the harness (batch + real-time pacing) | done | Claude/Charles | T02 | `WavFileSource` plays T03 fixtures; chunk size + realtime pacing configurable; covered in `tests/test_sources_and_corpus.py` |
| T05 | Virtual PipeWire source node feeding fixture audio at real-time rate (Phase 1) | todo | | T04 | Works headless in the Taipei lab; node teardown clean across runs |
| T06 | Accuracy metrics: WER/CER of `done` transcript vs reference, normalization rules documented | done | Claude/Charles | T03, T04 | `myna.testbed.metrics` (`word_error_rate`/`character_error_rate` with S/D/I breakdown + documented NFKC/casefold/punctuation normalization); pure, scoreable from stored records; `tests/test_metrics.py`. Used by `dev/bench.py` |
| T07 | First real adapter, commit-on-finalize (Phase 2): faster-whisper | done | Claude/Charles | T04 | `myna.testbed.whisper.FasterWhisperAdapter`; `whisper` uv extra; integration test skips cleanly without extra/model; `HF_HUB_OFFLINE=1` run verified; CPU default (GPU is explicit, mirroring engine selection) |
| T08 | Streaming adapter (Phase 3): whisper_streaming/LocalAgreement on faster-whisper, emitting progress/final incrementally | todo | | T07 | Partial-churn observable in ResultRecords; no retraction of finals |
| T09 | Nemotron adapter via NeMo (native streaming transducer), `att_context_size` sweepable | todo | | T07 | Same contract tests; latency dial exposed as candidate config |
| T10 | Qwen3-ASR adapter via vLLM (known patching pain — keep inside adapter) | todo | | T07 | Document required patches in the adapter dir; do not leak into harness |
| T11 | Matrix runner + result aggregation (Phase 4): sweep candidates × fixtures × configs, aggregate JSONL into comparison tables | in progress | Claude/Charles | T06, T08 | One command produces the model×hardware×strategy matrix. **Seeded:** `dev/bench.py` sweeps all fixtures against a live socket, scores WER/CER + latency, writes `results/bench.jsonl` tagged by `--label` (auto-detects active engine). Single-engine for now; multi-candidate aggregation + the cross-run comparison table remain. (Surfaced + fixed an adapter bug: faster-whisper rejects BCP-47 `en-GB` — normalized to ISO 639-1 in the adapter) |
| T12 | Hardware tier report from Taipei lab runs | todo | | T11 | Written tiers proposal delivered to IE114/UD129 owners (feeds T19) |
| T25 | Real recorded-speech corpus tier: redistributable human recordings (e.g. LibriSpeech/Common Voice subsets) added to the manifest, covering accents and noise authentically | todo | | T03 | Synthetic espeak audio is fine for plumbing/latency but not for real-world accuracy claims; licensing recorded per clip |

## Workstream B — ASR inference snap

Reference material: `reference/qwen3-snap` and `reference/gemma4-snap`
(snapcraft layout, `engines/*/engine.yaml` + `server` scripts, components for
runtimes/models, install hooks for hardware selection), and
`reference/inference-snaps-cli` (`modelctl`, IE108 CLI compliance).
Design note: `docs/asr-inference-snap-design.md` (T13 output).

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T13 | Study + design note: how qwen3/gemma4 snaps structure engines/components/hooks, and what an ASR snap does differently (audio-push server instead of llama.cpp server) | done | Claude/Charles | — | `docs/asr-inference-snap-design.md`. Decisions: one snap per family, Whisper first; copy gemma4's v2 engine/runtime/model schema; testbed adapter doubles as the snap's server; socket-activated UDS daemon; no audio interfaces needed |
| T14a | `myna.server`: standalone entry point wrapping an `SttService` adapter behind the real transport on a UDS path | done | Claude/Charles | T07, T16 | `myna-server --socket … --model …` (`src/myna/server/`); subprocess integration test in `tests/test_server.py`; verification client `dev/transcribe.py` |
| T14b | whisper-snap skeleton: snapcraft.yaml (core24), `cpu` engine, tiny/base/small models, `modelctl` v2 wiring, install hooks | done — installed & verified | Claude/Charles | T14a | `whisper-snap/`; installed, fixture clips transcribed through the snap socket; model selection (tiny→base) verified live. Warm finalize 0.5–0.8s on base CPU. Skeleton deviations: weights downloaded at runtime (components → T15), socket world-connectable (access control → T14c). Snapd findings recorded in design note (`network-bind` required for Unix `listen()`; `ws+unix` protocol unknown to `modelctl status`) |
| T14c | Socket exposure + access control for confined clients (content interface vs file perms + polkit) | todo | | T14b, T17 | Joint decision with T17; documented in design note |
| T15 | Engine variants + hardware selection (nvidia-gpu engine, remaining model components, VRAM-aware model gating follow-up with inference-snaps-cli team) | done | Claude/Charles | T14b, T12 | **Model components landed (CPU+GPU):** weights ship as `model-{tiny,base,small}` components via `dev/download-models.sh`; server loads `MODEL_DIR=$SNAP_COMPONENTS/<id>`; `network` plug dropped (no runtime download). Runtime delivery: cpu venv in base, CUDA as `faster-whisper-cuda` component. **nvidia-gpu engine verified on hardware 2026-06-13** (auto-selection scores GPU>CPU; CUDA libs resolve via runtime.yaml LD_LIBRARY_PATH). Sideload requires `.comp` files in the same `snap install` (documented in README). Remaining-but-deferred: larger GPU weights (medium/large-v3) + VRAM-aware per-model gating, both gated on T12 sizing (upstream gap #1, design note §4) |

## Workstream C — Transport (IE114 wire protocol)

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T16 | Prototype WebSocket-over-UDS client/server implementing the session contract; PCM binary frames in, JSON events out | done | Claude/Charles | T02 | `myna/core/transport_ws.py`; contract tests parametrized over loopback + ws-uds; wire protocol documented in module + ADR, feeds T18 |
| T17 | Access-control spike: socket permissions, snap confinement, client identity (polkit?) per IE114 comments | todo | | T16 | Documented recommendation, not necessarily code |

## Workstream D — Desktop client (UD129)

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T20 | Audio Adapter: PipeWire capture → bounded in-memory buffer → `PcmChunk` stream; VAD optional first pass | todo | | T01 | No disk persistence; buffer discarded on session end; unit tests with fake capture |
| T21 | Session controller: hotkey press/release driving the `DictationState` machine, wiring audio → `SttSession` → text output | todo | | T16, T20 | State transitions validated against `TRANSITIONS`; error states surface user feedback |
| T22 | IBus `TextInjector` backend (commit-only MVP) + activity indicator; end-to-end dictation demo against the inference snap | todo | | T14, T21 | UD129 acceptance: commit-only insertion, indicator lifecycle, secure-field blocking where detectable |
| T23 | Post-processing pass 1: capitalization/punctuation gap analysis per model (Nemotron has it native; Whisper mostly; what remains?) | todo | | T07 | Decide what post-processing the MVP actually needs before building it |

## Workstream E — Spec feedback

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T18 | IE114 update proposal: audio-push model, WebSocket transport, event vocabulary (progress/final/done/error), error model with stable codes | todo | | T16 | Draft for Farshid; informed by T16 prototype findings |
| T26 | Spec question: a session-lifecycle / `session.starting` signal so clients can show "loading model…" distinctly from "transcribing". Surfaced by T14b — cold model load (download) is a long silent gap that textless `progress` heartbeats only partially address; relates to IE114 comment [h] (session.starting/listening/transcribing states) | todo | | T18 | Recommendation for Farshid: keep the dumb-`progress` vocabulary, or add a lifecycle/status event? |
| T19 | Performance contract proposal: latency SLOs grounded in measured testbed numbers, per hardware tier | todo | | T12 | Draft for IE114/UD129 owners ahead of a Wednesday sync |
| T24 | Capabilities-discovery API sketch (models, languages, punctuation support) | todo | | T18 | Needed before Settings UI work can be scoped |

---

## Milestones

1. **M0 — Contract verified (done, 2026-06-12):** T01–T02. Fake adapter +
   harness over loopback; contract tests green.
2. **M1 — Real audio, real model:** T03–T07. One real candidate measured
   end-to-end on fixture audio.
3. **M2 — Streaming matrix:** T08–T11. Streaming candidates compared;
   evaluation matrix produced.
4. **M3 — Spec convergence:** T16, T18, T19, T12. IE114 updated with
   transport, events, and measured performance contract.
5. **M4 — End-to-end dictation:** T14a–T14c, T20–T22. Hotkey → speech → text
   in a GNOME app via the whisper snap.
