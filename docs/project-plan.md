# Myna Project Plan

**Created:** 2026-06-12 · **Last updated:** 2026-06-14
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
| T09 | Nemotron adapter via NeMo (native streaming transducer), `att_context_size` sweepable | done (commit-on-finalize) | Claude/Charles | T07 | `myna.testbed.nemotron.NemotronAdapter`; `nemotron` extra (`nemo_toolkit[asr]`); `att_context_size` latency dial in the candidate label; `myna-server --adapter nemotron` so `bench.py` measures it over a socket (no snap). **Verified 2026-06-14:** decode works first try (no API iteration needed); real-speech dictation via `dev/dictate.py` is perfect and near-instant. See *Measured findings* — finalize ~0.027s (native transducer wins latency decisively), but synthetic WER is unreliable (espeak is OOD for it). English-only (`non-english-german` 100%). Native frame-at-a-time streaming is the follow-up (converges with T08) |
| T10 | Qwen3-ASR adapter via vLLM (known patching pain — keep inside adapter) | todo | | T07 | Document required patches in the adapter dir; do not leak into harness |
| T11 | Matrix runner + result aggregation (Phase 4): sweep candidates × fixtures × configs, aggregate JSONL into comparison tables | done (single-strategy) | Claude/Charles | T06, T08 | `dev/bench.py` sweeps fixtures against a live socket → `results/bench.jsonl`; `dev/aggregate.py` micro-averages WER/CER + finalize percentiles into a cross-`--label` table (overall + by-category), deduped by (label, clip); `dev/run-matrix.sh` drives engine/model switches → bench → aggregate in one command. Covers the model×hardware axis; the **streaming-strategy axis is added when T08 lands** (only commit-on-finalize exists today). Surfaced + fixed the `en-GB` adapter bug |
| T12 | Hardware tier report from Taipei lab runs | todo | | T11 | Written tiers proposal delivered to IE114/UD129 owners (feeds T19) |
| T25 | Real recorded-speech corpus tier: redistributable human recordings (e.g. LibriSpeech/Common Voice subsets) added to the manifest, covering accents and noise authentically | done (clean+noise tier; accents follow-up) | Claude/Charles | T03 | `dev/fetch_real_corpus.py` downloads LibriSpeech `dev-clean` (CC-BY-4.0), decodes FLAC→16k mono WAV (ffmpeg), and writes `corpus/real/` (schema-v1 manifest, per-clip `source`/`license`, NOTICE for attribution). Selection is deliberately trivial (first N utterances in archive order) — **kept simple at Charles's request**; 12 `quiet` clips + 2 `noise` variants (real speech + seeded SNR-10, reusing the synthetic mixer). **Regenerated, not committed** — gitignored like `fixtures/`; dev takes the ~337 MB download hit (cached under gitignored `.cache/`). **Thesis verified 2026-06-14:** same Nemotron model scores **0.0% WER on the real clips** vs **44.6% on synthetic espeak** (100% on the pangram — empty output) — synthetic WER was indeed misleading; real WER is now trustworthy. Unblocks T12 (tiers) and T19 (perf contract). **Honest scope:** trivial first-N selection means low speaker variety (one speaker at the default N), and dev-clean is clean read speech — so this covers real-voice + noise authentically but **not accents/speaker diversity**; an accent-labelled corpus (Common Voice/VCTK; credentialed/large download) is the follow-up |

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
| T27 | Model idle-unload (in-process): `myna-server --sleep-idle-seconds`, `adapter.unload()` releasing model weights + GPU memory, idle monitor in the server | done | Claude/Charles | T14a | **Done + wired into the snap:** `--sleep-idle-seconds N --idle-action unload`; `unload()` on both adapters (drop ref + gc; NeMo also `torch.cuda.empty_cache()`); `LifecycleService` + `idle_monitor` (`myna/server/lifecycle.py`). Engine `server` scripts pass `--sleep-idle-seconds "$(modelctl get sleep-idle-seconds)" --idle-action unload`; install hook default 300s. Frees the **weights** (the bulk: ~1.1 GB of 1.4 GB for NeMo); process + CUDA context (~300 MB) stay (full release blocked — see T28). Re-warm faster than cold; `progress.phase=preparing` covers the UX. Verify via `nvidia-smi` |
| T28 | snapd socket activation: snapd owns the UDS, daemon starts on first connect and **exits after idle**, relaunched on next connection — full process/VRAM release at idle | blocked (upstream `modelctl run`) | Claude/Charles | T14b, T27, T26 | **Server mechanism done + dev-testable** (`serve_unix` serves on a `systemd_socket()` fd; `--idle-action exit`; verified on a pre-bound socket; runs under `systemd-socket-activate -- .venv/bin/myna-server …`). **Blocked for the snap:** `modelctl run` forks the server and doesn't pass fd 3, so the activation handoff can't reach `myna-server` (design note §4). Snap ships T27 in-process unload instead. Unblock = upstream `modelctl run` change (`syscall.Exec` or forward the listening fd + `LISTEN_PID`). Wake-cost note (Charles, 2026-06-14): a few seconds, not great → GPU parsimony via T27 prioritised for now |
| T32 | `nemotron-snap`: package the Nemotron adapter as an inference snap (one snap per family), sibling of whisper-snap | done | Claude/Charles | T09, T15 | `nemotron-snap/` mirrors whisper-snap: `nvidia-gpu` engine, **`nemo-cuda` runtime component** (myna[nemotron] = NeMo + torch + CUDA pip tree — multi-GB), `.nemo` model component (`model-streaming-multi`), `att-context-size` dial exposed as engine config, idle-unload wired (T27). Adapter restores a local `.nemo` via `restore_from`. **Verified in strict confinement on hardware 2026-06-14:** NeMo+torch+CUDA load, `LD_LIBRARY_PATH` resolves first try, `.nemo` restores, server serves. Benign warnings (no action): pydub/ffmpeg (we feed raw PCM, never decode files) and joblib serial-mode (confinement blocks its shm probe). Socket activation blocked (T28) so it ships in-process unload |
| T29 | Residency **default policy** (adaptive): define and implement the v1 default everyone runs under — idle-unload timeout, per-engine warm-vs-cold strategy (whisper full-exit cheap-wake vs NeMo keep-warm), with memory-pressure / power-source awareness as the growth path | todo | | T27, T28 | **Name this first** — the dev toggles (T30) and any future user control are deviations *from* this baseline. 95% of users live here and never touch a knob, so the default is the product. Eventual user-facing control exposes *intent* ("keep dictation instantly ready" vs "free memory when idle"), never mechanism, and lands in UD129 (desktop Settings) scope, designed later from evidence not guesses |
| T30 | Development/test lifecycle toggles: deterministic controls to force residency states (unload-now, pin-resident, set idle-timeout, force-cold-wake) so engineering can test scenarios in the wild; hidden, not user-facing UI | todo | | T27 | **Two shapes, do not conflate:** (a) *test control* — imperative, ephemeral, forces exact states for engineering; (b) *power-user out* — declarative, persisted config (modelctl/IE108 scope), undocumented-but-reachable. Caveat for the "gather data to design the UI later" rationale: it collides with the offline/privacy posture (no content, no default phone-home), the only reachable population is power users (a biased sample for a mainstream UI), and reachable-but-hidden is still a production contract (Hyrum). Treat UI-informing as a weak secondary benefit; justify the work on (a) |

## Workstream C — Transport (IE114 wire protocol)

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T16 | Prototype WebSocket-over-UDS client/server implementing the session contract; PCM binary frames in, JSON events out | done | Claude/Charles | T02 | `myna/core/transport_ws.py`; contract tests parametrized over loopback + ws-uds; wire protocol documented in module + ADR, feeds T18 |
| T17 | Access-control spike: socket permissions, snap confinement, client identity (polkit?) per IE114 comments | todo | | T16 | Documented recommendation, not necessarily code |

## Workstream D — Desktop client (UD129)

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T20 | Audio Adapter: PipeWire capture → bounded in-memory buffer → `PcmChunk` stream; VAD optional first pass | in progress | Claude/Charles | T01 | No disk persistence; buffer discarded on session end; unit tests with fake capture. **Seeded:** `MicSource` (`myna.testbed.sources`) captures live PipeWire audio via `pw-record --raw` → `PcmChunk` stream, memory-only, `stop()`-terminated; `dev/dictate.py` is a working push-to-talk demo (speak → Enter → transcription) against the snap — the audio-push model end to end, verified live. Still to do for T20 proper: bounded ring buffer, VAD, `--target` virtual-node wiring (T05), fake-capture unit test |
| T21 | Session controller: hotkey press/release driving the `DictationState` machine, wiring audio → `SttSession` → text output | todo | | T16, T20 | State transitions validated against `TRANSITIONS`; error states surface user feedback |
| T22 | IBus `TextInjector` backend (commit-only MVP) + activity indicator; end-to-end dictation demo against the inference snap | todo | | T14, T21 | UD129 acceptance: commit-only insertion, indicator lifecycle, secure-field blocking where detectable |
| T23 | Post-processing pass 1: capitalization/punctuation gap analysis per model (Nemotron has it native; Whisper mostly; what remains?) | todo | | T07 | Decide what post-processing the MVP actually needs before building it |

## Workstream E — Spec feedback

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T18 | IE114 update proposal: audio-push model, WebSocket transport, event vocabulary (progress/final/done/error), lifecycle signal (from T26) | todo (drafting) | Claude/Charles | T16, T26 | **Committed (2026-06-14):** audio-push model + WebSocket-over-UDS transport (working prior art in `transport_ws.py`) + the progress/final/done/error vocab. Deliverable: the written proposal for Farshid. **Error model carved out to T31** — it was the genuinely-unresolved third and shouldn't block the transport/vocab draft |
| T31 | Stable error-code taxonomy for IE114: enumerated codes with semantics (terminal vs recoverable, client vs server fault, retryable), replacing the ad-hoc adapter strings (`unsupported_audio_format`, `inference_failed`) | todo | | T18 | The part IE114 itself flagged incomplete. Two codes exist in adapter code, not a spec. Needs the full set + meaning before T18 can claim a complete error model |
| T26 | Spec decision: add a session-lifecycle signal so clients show "loading model…" distinctly from "transcribing" | resolved (2026-06-14) | Claude/Charles | T18 | **Decision: yes, add a lifecycle signal** — `progress` conflates "model loading, nothing happening yet" with "transcribing, almost done", and the cold-load measurements (0.9–2.2 s whisper, more for NeMo) make that gap real. **Scoped, though:** the audio-push model collapses IE114 comment [h]'s `starting/listening/transcribing` trichotomy — the client owns capture so "listening" is redundant and "transcribing" == `progress`. So **one** new phase ("preparing"/model-loading), not a 3-state FSM (that would repeat the over-spec'ing the vocab simplification rejected). **Implemented as `progress.phase` field** (`preparing`/`transcribing`, default transcribing) — not a new event; adapters tag the load-heartbeat `preparing`, `dev/dictate.py` shows "loading model…" distinctly; field round-trips and is forward/backward compatible across the wire |
| T19 | Performance contract proposal: latency SLOs grounded in measured testbed numbers, per hardware tier | todo | | T12 | Draft for IE114/UD129 owners ahead of a Wednesday sync |
| T24 | Capabilities-discovery API sketch (models, languages, punctuation support) | done | Claude/Charles | T18 | `myna.core.capabilities.Capabilities` (models, languages, `input_formats`, punctuation, translation) + wire codec; `SttService.capabilities()` / `SttClient.capabilities()` served over **both** transports (loopback direct; WS answers a `capabilities.query` message, parametrized contract test proves wire parity); all three adapters populate it (whisper multilingual/punctuation; nemotron en-only/native-punct; fake trivial). `dev/capabilities.py` queries a live snap. **Folds in the audio-format advertisement** (`input_formats`): the service states what PCM it accepts, the client delivers it, and the adapters now **reject** off-format audio instead of resampling — the `np.interp` blocks are gone from both adapters (symmetric with the existing channels/width rejection; conversion is the client's job under audio-push). Provisional vocab → feeds T18 |

---

## Measured findings (synthetic fixture tier, 2026-06-14)

First model×hardware matrix on the synthetic espeak corpus (13 clips, commit-on-finalize).
Latency is content-independent and **trustworthy**; WER is **directional only** and, as
point 3 shows, actively misleading across architectures.

| config | WER% | median finalize | cold load | note |
|---|---|---|---|---|
| `nvidia-gpu/small` | 11.07 | 0.150s | 0.91s | only config meeting both accuracy + UD129 latency |
| `cpu/small` | 11.07 | 1.685s | 2.21s | p90 3.4s > UD129 1–2s target — `small` not viable on CPU for live |
| `cpu/tiny` | 37.27 | 0.295s | 0.51s | fast but inaccurate |
| `nemotron` (default ctx) | 59.04 | 0.027s | — | native transducer — fastest by far; synthetic WER unreliable (point 3) |

1. **Native transducer wins latency decisively.** Nemotron finalize ~0.027s (each frame
   processed once, no end-of-utterance re-decode) vs AED Whisper 0.15s (GPU) / 1.7s (CPU).
2. **Whisper CPU vs GPU WER is identical** (11.07%, bit-for-bit per category): GPU is a pure
   latency play, not accuracy.
3. **Synthetic audio cannot fairly compare architectures.** Nemotron scores 59% WER on
   espeak yet transcribes real human speech perfectly (verified via `dev/dictate.py`). espeak
   is severely OOD for models trained on real speech; Whisper's web-scale training masks this.
   → real WER claims need a recorded-speech corpus (**T25, now critical path**); synthetic tier
   stays valid for plumbing + latency only.
4. Nemotron is **English-only** (`non-english-german` 100%, as predicted).

## Real-corpus finding (LibriSpeech tier, 2026-06-14)

First run on the **real recorded-speech** tier (T25, `corpus/real/`), Nemotron,
commit-on-finalize, vs the synthetic espeak tier under the *same model and code path*:

| tier | clips | mean WER% |
|---|---|---|
| real (LibriSpeech dev-clean, clean read speech) | 6 | **0.0** |
| synthetic (espeak, quiet + long-form) | 3 | **44.6** |

The gap is the whole point of T25: the synthetic-tier WER (44.6%, incl. 100% / empty
output on the pangram) is an artefact of espeak being out-of-distribution, **not** a
model deficiency — the same model is flawless on real voice. WER claims must come from
the real tier; the synthetic tier remains valid for plumbing + latency only. (Accents
still uncovered — clean read speech only; Common Voice/VCTK is the follow-up.)

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
