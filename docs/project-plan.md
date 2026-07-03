# Myna Project Plan

**Created:** 2026-06-12 · **Last updated:** 2026-07-02
**Status:** Living document — update task status in place as work lands.

This plan turns IE114 (UbuSTT API), UD129 (Desktop STT Integration), and the
testbed phasing from CLAUDE.md into assignable tasks. Task IDs are stable;
reference them in branches and PRs (e.g. `t02-fake-adapter`).

**IE115 (2026-06-17):** a competing braindump proposing a WebSocket API shaped
on OpenAI's Realtime API has appeared (`IE115-spec.txt`). It does *not* yet
supersede IE114 — it is still a braindump. It happens to ratify the two pivots
we already made against IE114 (audio-push + WebSocket), but imports a lot of
OpenAI speech-to-speech baggage that a local transcriber doesn't want.
Reconciliation is **Workstream F**; our push-backs live in
`docs/IE115-deviations.md`.

**Update (2026-07-01):** Workstream F is resolved. The team decided IE115 will be
a *suitable subset* of OpenAI's Realtime API (compatibility + remote-backend +
industry-contribution reasons), extended with additive events. Our liveness event
and capabilities discovery were adopted; the flat-profile and drop-conversation-
item push-backs were overruled for compatibility; translation is out of scope.
Full mapping in `docs/IE115-resolution.md`; async lifecycle diagrams in
`docs/architecture/ie115-lifecycle.md`. Remaining open: error taxonomy (T31),
protocol versioning (T35), overload-lag signal, GPU memory pressure. Next focus
shifts to the **orchestrator subsystem** (Charles) and the inference snap server
(Ivano).

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
- **F — IE115 reconciliation**: integrate the IE115 WebSocket proposal where it
  improves on IE114, and document where we deviate from its OpenAI-Realtime
  lineage. Feeds E (it *is* spec work, but kept separate so the IE115 decisions
  get their own owners and don't get lost inside the IE114-update task).
- **G — Orchestrator subsystem (Rust)**: the client-side dictation brain — the
  session + model-residency FSM from `docs/architecture/ie115-lifecycle.md`,
  mediating the audio adapter (D/Matias), the inference snap (B/Ivano), the
  hotkey (T21) and the injector (T22). Every external boundary is a trait with a
  mock, so it is buildable now: the existing Python `myna-server` stands in for
  the inference snap, a WAV source for the audio adapter, stdin for the hotkey,
  stdout for the injector.

Dependencies at a glance: A is unblocked now and feeds E; C unblocks B and D's
integration work; B and D converge in the end-to-end milestone (T22); G consumes
B and D behind interfaces (stubbed until they land) and builds directly against
the lifecycle diagram.

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
| T08 | Streaming adapter (Phase 3): whisper_streaming/LocalAgreement on faster-whisper, emitting progress/final incrementally | todo | | T07 | Partial-churn observable in ResultRecords; no retraction of finals. **Design note drafted 2026-07-03** (`docs/architecture/streaming.md`): the gating decision is the *revision contract* — recommends **append-only committed** (deltas/finals never retracted; whisper emits coarser/later segments rather than churning; unstable motion stays on `snippet`). Segmentation-only streaming (earlier `final`s) needs no `PROTOCOL_VERSION` bump; a committed-`delta` event does. Adds two testbed metrics (`time_to_first_committed`, `commit_stability`). Awaiting §3 ratification |
| T09 | Nemotron adapter via NeMo (native streaming transducer), `att_context_size` sweepable | done (commit-on-finalize) | Claude/Charles | T07 | `myna.testbed.nemotron.NemotronAdapter`; `nemotron` extra (`nemo_toolkit[asr]`); `att_context_size` latency dial in the candidate label; `myna-server --adapter nemotron` so `bench.py` measures it over a socket (no snap). **Verified 2026-06-14:** decode works first try (no API iteration needed); real-speech dictation via `dev/dictate.py` is perfect and near-instant. See *Measured findings* — finalize ~0.027s (native transducer wins latency decisively), but synthetic WER is unreliable (espeak is OOD for it). English-only (`non-english-german` 100%). Native frame-at-a-time streaming is the follow-up (converges with T08) |
| T10a | Qwen3-ASR adapter via the **pure-C runtime** (antirez/qwen-asr) through ctypes FFI | done (commit-on-finalize) | Claude/Charles | T07 | `myna.testbed.qwen.QwenCAdapter`; **zero pip deps** (ctypes stdlib + `libqwen_asr.so`, int16→f32 via stdlib `array`) — the parsimonious, multilingual (30 langs), no-GPU runtime. `myna-server --adapter qwen-c --model <dir>`; lib via `QWEN_ASR_LIB`. **Verified 2026-06-15** end-to-end through `run_session` (FFI load+decode correct on real clips, 0% WER) + 16 offline unit tests (`tests/test_qwen_unit.py`). Snapped (qwen-snap `cpu` engine, builds the `.so` from source). Probe (Ryzen 9 7950X3D, optimistic ceiling): offline ~1.4–1.6s warm for 5–6s clips (edge of UD129 target; over budget on a laptop), streaming 1.86x realtime (sub-realtime on weaker CPUs → live streaming is a follow-up, not MVP). Native streaming has a monotonic commit frontier matching our `final` contract when revisited. **GPU "best quality" runtime (vLLM, T10b) is on the `qwen3-vllm-gpu` branch** (verified out-of-confinement, snap parked — see that branch's `docs/qwen-vllm-confinement.md`); proves runtimes are switchable per family via the engine mechanism |
| T11 | Matrix runner + result aggregation (Phase 4): sweep candidates × fixtures × configs, aggregate JSONL into comparison tables | done (single-strategy) | Claude/Charles | T06, T08 | `dev/bench.py` sweeps fixtures against a live socket → `results/bench.jsonl`; `dev/aggregate.py` micro-averages WER/CER + finalize percentiles into a cross-`--label` table (overall + by-category), deduped by (label, clip); `dev/run-matrix.sh` drives engine/model switches → bench → aggregate in one command. **Config-driven multi-backend runner added 2026-07-02:** `dev/matrix.py` + `dev/matrix.yaml` sweep *targets* (whisper/nemotron/qwen) each provisioned on a socket — provisioner `server` (spawns `myna-server` itself: local-first, no snap/sudo) or `snap` (drives an installed snap: engine/model switch + restart). Per target it takes a **cold** sample (model-load-from-cold, `--cold`) then a **warm** sweep, stamps `hardware` provenance onto every record (machine/cpu/gpu/tier, T12), and calls aggregate. New metrics wired through: `time_to_ready` (cold load), `rtf`, p50/p95 finalize; aggregate shows a machine column + distinct cold-load column. Per-`server`-target **peak RSS/VRAM** sampled (psutil + nvidia-smi) into a `matrix-resources.jsonl` sidecar and shown as columns; server logs redirected to `results/matrix-logs/`. Verified locally (no snaps): whisper-base/cpu 9.14% WER / 640 MB, qwen-c/cpu 1.52% / 2.8 GB, nemotron/cuda 0.51% / 1.15 GB VRAM (RTF 0.01, 25 ms finalize, 5.8 s cold load). Covers the model×hardware axis; the **streaming-strategy axis is added when T08 lands** (only commit-on-finalize exists today). Surfaced + fixed the `en-GB` adapter bug |
| T12 | Hardware tier report from Taipei lab runs | todo | | T11 | Written tiers proposal delivered to IE114/UD129 owners (feeds T19). **Unblocked 2026-07-02:** `dev/matrix.py` now stamps machine/cpu/gpu/tier provenance and captures cold-load + RTF per target, so a lab node just runs `matrix.py` with its `hardware:` filled in and ships the JSONL |
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
| T20 | Audio Adapter: PipeWire capture → bounded in-memory buffer → `PcmChunk` stream; VAD optional first pass | in progress | Claude/Charles | T01 | No disk persistence; buffer discarded on session end; unit tests with fake capture. **Seeded:** `MicSource` (`myna.testbed.sources`) captures live PipeWire audio via `pw-record --raw` → `PcmChunk` stream, memory-only, `stop()`-terminated; `dev/dictate.py` is a working push-to-talk demo (speak → Enter → transcription) against the snap — the audio-push model end to end, verified live. Still to do for T20 proper: bounded ring buffer, `--target` virtual-node wiring (T05), fake-capture unit test. **Shape decisions pending the audio-adapter meeting** (`docs/audio-adapter-meeting-prep.md`): recommend **library (not daemon)** in-process with the dictation service, exposing the existing `AudioSource`→`PcmChunk` interface; DSP stays in PipeWire + model; **VAD dropped from the MVP** (push-to-talk = the hotkey is the VAD); the one real job is device/channel selection + bounded buffering + conversion to the negotiated `input_format` (T33) |
| T21 | Session controller: hotkey press/release driving the `DictationState` machine, wiring audio → `SttSession` → text output | todo | | T16, T20 | State transitions validated against `TRANSITIONS`; error states surface user feedback. **Hotkey mechanism resolved (2026-06-18, source- + expert-verified):** `org.freedesktop.portal.GlobalShortcuts` delivers `Activated` (press) + `Deactivated` (release) → **hold-to-talk works, no shell extension** (chain verified through xdg-desktop-portal-gnome → gnome-shell → mutter, upstream not an Ubuntu patch; GNOME 50). PTT key = a free 2-key `Super+<letter>` (no modifier-only — breaks apps, per Marco/3v1n0), rebindable via portal `preferred_trigger` + Settings; client dedupes autorepeat `Activated`. **Open:** pick the default `Super+<letter>` (enumerate taken combos via `gsettings`, ask Marco which letters are reserved) + confirm the Settings "customize hotkey" ↔ portal binding flow. See `docs/audio-adapter-meeting-prep.md`. **Greenfield → language gated** (write in Go/Rust pending the policy decision, see `docs/python-scope-justification.md`) |
| T22 | IBus `TextInjector` backend (commit-only MVP) + activity indicator; end-to-end dictation demo against the inference snap | todo | | T14, T21 | UD129 acceptance: commit-only insertion, indicator lifecycle, secure-field blocking where detectable |
| T23 | Post-processing pass 1: capitalization/punctuation gap analysis per model (Nemotron has it native; Whisper mostly; what remains?) | todo | | T07 | Decide what post-processing the MVP actually needs before building it |

## Workstream E — Spec feedback

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T18 | IE114 update proposal: audio-push model, WebSocket transport, event vocabulary (progress/final/done/error), lifecycle signal (from T26) | done (draft, for review) | Claude/Charles | T16, T26 | **Draft delivered 2026-06-17:** `docs/IE115-proposal.md` — discrete anchored comments (one per IE115 section) + position paragraph, for Charles to paste into the IE115 Google Doc as comments. Folds in the deviations doc, the implemented versioning/handshake (T35), the progress/final/done/error vocab (T36), capabilities (T24), and the `preparing` phase (T26). **Committed (2026-06-14):** audio-push model + WebSocket-over-UDS transport (working prior art in `transport_ws.py`) + the progress/final/done/error vocab. Deliverable: the written proposal for Farshid. **Error model carved out to T31** — it was the genuinely-unresolved third and shouldn't block the transport/vocab draft. **2026-06-17:** IE115 landed and independently validates the transport + push direction here; the IE115-specific reconciliation (versioning, event-name alignment, deviations note) is split into Workstream F (T34–T37) so this task stays the IE114-targeted update and the IE115 decisions get their own owners |
| T31 | Stable error-code taxonomy for IE114: enumerated codes with semantics (terminal vs recoverable, client vs server fault, retryable), replacing the ad-hoc adapter strings (`unsupported_audio_format`, `inference_failed`) | todo | | T18 | The part IE114 itself flagged incomplete. Two codes exist in adapter code, not a spec. Needs the full set + meaning before T18 can claim a complete error model |
| T26 | Spec decision: add a session-lifecycle signal so clients show "loading model…" distinctly from "transcribing" | resolved (2026-06-14) | Claude/Charles | T18 | **Decision: yes, add a lifecycle signal** — `progress` conflates "model loading, nothing happening yet" with "transcribing, almost done", and the cold-load measurements (0.9–2.2 s whisper, more for NeMo) make that gap real. **Scoped, though:** the audio-push model collapses IE114 comment [h]'s `starting/listening/transcribing` trichotomy — the client owns capture so "listening" is redundant and "transcribing" == `progress`. So **one** new phase ("preparing"/model-loading), not a 3-state FSM (that would repeat the over-spec'ing the vocab simplification rejected). **Implemented as `progress.phase` field** (`preparing`/`transcribing`, default transcribing) — not a new event; adapters tag the load-heartbeat `preparing`, `dev/dictate.py` shows "loading model…" distinctly; field round-trips and is forward/backward compatible across the wire |
| T19 | Performance contract proposal: latency SLOs grounded in measured testbed numbers, per hardware tier | todo | | T12 | Draft for IE114/UD129 owners ahead of a Wednesday sync |
| T24 | Capabilities-discovery API sketch (models, languages, punctuation support) | done | Claude/Charles | T18 | `myna.core.capabilities.Capabilities` (models, languages, `input_formats`, punctuation, translation) + wire codec; `SttService.capabilities()` / `SttClient.capabilities()` served over **both** transports (loopback direct; WS answers a `capabilities.query` message, parametrized contract test proves wire parity); all three adapters populate it (whisper multilingual/punctuation; nemotron en-only/native-punct; fake trivial). `dev/capabilities.py` queries a live snap. **Folds in the audio-format advertisement** (`input_formats`): the service states what PCM it accepts, the client delivers it, and the adapters now **reject** off-format audio instead of resampling — the `np.interp` blocks are gone from both adapters (symmetric with the existing channels/width rejection; conversion is the client's job under audio-push). Provisional vocab → feeds T18 |
| T33 | **Team discussion (undecided):** sample *encoding* in the audio format — should `AudioFormat`/`input_formats` carry int16-vs-float32, so int16→float32 moves to the client (capture-native or edge-convert) and adapters only reinterpret, never convert? | todo (discuss) | | T24 | **No decision yet — bring to the team.** Finding (2026-06-16): `AudioFormat` has no encoding field today (width only; wire is implicitly S16LE, float32 not expressible). At the raw-frame API **all the adapters want the *same* thing** — float32 normalised [-1,1] 16k mono — and each does the identical `int16→float32/32768` (whisper/nemotron/qwen-c). int16 ingestion exists only at file/stdin decode boundaries we don't use, and converts to float internally anyway. **Implications pull both ways:** uniform target ⇒ a single wire encoding (keep s16le, or switch to f32le + convert once in `MicSource`) settles it without per-model negotiation; and since ASR universally wants float, a full capabilities *encoding-negotiation* axis looks premature (solves a divergence that doesn't exist). Open question = which wire encoding + where the one conversion lives (adapter today vs source/edge per the audio-push invariant). Don't over-build until a model actually diverges. Analysis: this session's grilling |

## Workstream F — IE115 reconciliation

IE115 (`IE115-spec.txt`) is a WebSocket transcription API modelled on OpenAI's
Realtime API. It ratifies our two pivots away from IE114 — **audio-push** (the
client streams PCM up, no `pipewire-node-name`) and **WebSocket** (the forced
consequence of audio-push: SSE can't carry a client→server audio stream). The
work here is *not* a rebuild — our `myna.core` is already closer to IE115 than
to IE114 — it is renaming/negotiation plus stripping the OpenAI speech-to-speech
baggage (conversation-item object graph, 24 kHz, base64-in-JSON, server VAD,
`obfuscation`/`usage`, voice/tools/instructions) that a local transcriber
doesn't want. See `docs/IE115-deviations.md` for the full keep/strip/modify
rationale.

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T34 | Deviations note (`docs/IE115-deviations.md`): per-feature keep/strip/modify position vs OpenAI Realtime, each justified by an invariant or a measured number | done | Claude/Charles | — | Landed + committed 2026-06-17. Six push-backs (no conversation model; 16 kHz via capabilities not fixed 24 kHz; binary frames not base64; server VAD optional not mandatory; drop `obfuscation`/`usage`; strip s2s config) + conscious decisions on what IE115 drops (segments/score, prompt, model-loading phase, reconnect). This is the team-facing artefact; IE115's own "Deviations from OpenAI Realtime API" section lists only two trivial ones |
| T35 | Protocol versioning: handshake-negotiated `protocol_version` (client states in `session.start`, server echoes in `session.created`) + a versioned event-vocabulary set | done | Claude/Charles | T34, T16 | **Done 2026-06-17.** `myna.core.protocol` (`PROTOCOL_VERSION="1"`, `SUPPORTED_PROTOCOL_VERSIONS`, `is_supported`). **Decided field-in-handshake, not subprotocol token** — versioning must be transport-agnostic (the invariant), so it travels in band in `session.start` and works over loopback/ws/future transports, not just WS. Client declares `protocol_version`; server validates → terminal `transcription.error(unsupported_protocol_version)` on mismatch (feeds T31), else acks `session.created` echoing the served version (captured on `_WsSession.protocol_version`). Missing version = compatible (pre-versioning clients). Single number versions the whole contract incl. the event vocab (additive event types are breaking — `event_from_wire` rejects unknown). `capabilities.query` kept (T24). Tests: `tests/test_protocol_version.py` (3, ws-specific — loopback can't disagree); contract suite green over both transports |
| T36 | Event-vocabulary reconciliation: keep `transcription.*` vs adopt OpenAI `conversation.item.input_audio_transcription.*` names; reconcile our `progress.snippet` (liveness, uncommitted) with IE115 `delta` (incremental *committed* text) | done | Claude/Charles | T34 | **Decided 2026-06-17 — keep flat `transcription.*`.** No code rename; the decision *is* the deliverable, documented in `core/events.py` with the IE115/OpenAI → ours mapping (delta→`progress` *snippet-as-liveness, not committed*; completed→`final`+`done`; failed→`error`; single error channel). `progress.phase=preparing` (T26) preserved — IE115 has no model-loading event. Committed-delta stream deferred to streaming (T08): we emit no committed deltas yet, so don't build the semantics until a model needs them. Vocab versioned via `PROTOCOL_VERSION` (T35) |
| T37 | Session lifecycle/config envelope: align `SessionConfig`/`session.start` with an IE115-shaped `session.update`/`session.created` exchange, stripped of s2s fields (voice, tools, instructions, output_modalities, create_response) | done | Claude/Charles | T34, T24 | **Decided 2026-06-17 — keep flat config, add the `session.created` ack (via T35).** No nested `session.audio.input.transcription` envelope: that nesting only exists to co-locate s2s output config (voice/speed) we don't have. Existing fields (language, output_language, prompt, timestamp_granularity) cover transcription; decision documented in `core/session.py`. No `turn_detection` field — turn detection is client-driven via `session.finish`, never server VAD (deviations §1.4). `session.created` handshake reply landed in T35. **Superseded for the wire (2026-07-02, T44):** the team overruled the flat-config pushback for compatibility, so the IE115 *dialect* (T44–T47) speaks the nested envelope on the wire; the internal `SessionConfig` stays flat and a codec translates. This task's decision stands for the *internal* vocab |

### IE115 wire dialect — hands-on implementation (from 2026-07-02)

Decided (Charles, 2026-07-02): implement IE115 on **both** the Python
`myna-server` and the Rust client ourselves — not waiting for Ivano — to get
concrete experience feeding the open design questions (error taxonomy, overload
signal, STATUS shape, object graph). Built as a **selectable wire dialect**: the
flat `myna.core` vocab stays the semantic core (contract tests untouched); a
codec translates flat ↔ OpenAI-nested on each end; dialect chosen by **shape-sniff**
(first frame `session.update` = IE115, `session.start` = internal). This cashes in
the wire-agnostic FSM (T40) and unblocks the Rust IE115 client (T43) without
Ivano's server. Contract in `docs/architecture/ie115-wire.md`.

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T44 | IE115 wire-dialect design note: exact frame contract (session/config/event mapping, STATUS liveness, error mapping, base64/binary hatch) + open questions | done | Claude/Charles | T34, T36, T37 | `docs/architecture/ie115-wire.md` (2026-07-02). Pins every frame both directions, realises the `events.py` mapping, flags 8 open questions code will force (dialect selection→**shape-sniff, decided**; format completeness; `done` semantics; STATUS shape; overload/lag; object-graph minimalism; `include` tokens; `prompt` placement). Takes the position `model_loading`-as-error is wrong → `STATUS{loading}` instead |
| T45 | Python `myna-server`: IE115 codec (`myna.core.wire_ie115`) + shape-sniff dialect dispatch in `transport_ws`; internal vocab untouched; wire-parity test suite mirroring `tests/test_contract.py` | done | Claude/Charles | T44 | **Done 2026-07-02.** `core/wire_ie115.py` (`Ie115Encoder`/`Ie115Decoder`, config + append codecs); `transport_ws` shape-sniffs the first frame (`session.update`=IE115, `session.start`=internal, `capabilities.query`=caps) and dispatches to a shared `_pump_session`; `WsUnixIe115Client` added. Server emits `session.created`/`session.updated`; binary + base64 `append` accepted; STATUS additive; synthesises `item_id`/`content_index`; `done` synthesised client-side on close. `tests/test_ie115_dialect.py` — 15 tests (codec units + end-to-end parity over the fake adapter; the lossy `adapter_crash`→`server_error` mapping is asserted as T31 evidence). Full suite 119 green. Smoke-tested against live whisper (binary + base64, exact transcript) |
| T46 | IE115 loopback demo + append micro-benchmark: Rust client ↔ Python server over IE115 across whisper/nemotron/qwen; measure per-`append` CPU/alloc for binary vs base64 (review comment `[h]`) | done | Claude/Charles | T45, T43 | **Done 2026-07-02.** Rust `myna-dictate --dialect ie115` ↔ Python IE115 server verified end-to-end across all three families (whisper/AED, nemotron/transducer, qwen-c/LLM-decoder) — exact transcripts, binary and `--base64-audio` paths identical, proving the dialect is genuinely model-agnostic. Append micro-bench (100 ms 16 k mono s16le chunk): base64+JSON = **1.35× wire inflation** and ~16 µs CPU/chunk (9 µs enc + 7 µs dec) vs ~0 for binary → +11 KB/s + ~160 µs/realtime-second. Negligible per session, real at fleet scale — recorded in `docs/architecture/ie115-wire.md` §5 as the argument for binary-default |

## Workstream G — Orchestrator subsystem (Rust)

The client-side coordinator: it owns the **two-region async FSM** from
`docs/architecture/ie115-lifecycle.md` — the per-connection session track
(CREATED→ACTIVE→FINALIZING→DONE) running orthogonally to model residency
(UNLOADED→LOADING→RESIDENT) — with the accept-gate (`ACTIVE ∧ RESIDENT`),
commit-drain (COMMIT ≠ done; wait for `done`), pre-ready-audio drop, and
terminal-vs-recoverable error mapping. The FSM is the deliverable; everything
else is plumbing around it.

**Buildable now via mocks (our earlier work):** the existing Python `myna-server`
(ws+unix, fake/whisper/nemotron/qwen adapters) stands in for the IE115 inference
snap; a WAV source over `corpus/real` for the audio adapter; stdin for the
hotkey; stdout for the injector. Design decisions (2026-07-01, Charles): keep the
FSM **wire-agnostic** and speak the *existing* `myna.core` wire first (real
end-to-end runs against a running Python server today), with **IE115 event names
as P4**; enhance the Python fake adapter to emit the full `STATUS` sequence so
the residency gate is honestly exercised; small `core/orchestrator/cli`
workspace; **rigorous** FSM (all async edge cases, not a thin loop); Python
server kept as a test dependency (Rust integration tests spawn it).

Stack: Rust (nightly present), tokio + tokio-tungstenite + serde. Honors the
invariants: never persist audio, bounded in-memory buffering, no content logged.

| ID | Task | Status | Owner | Depends on | Notes / acceptance |
|---|---|---|---|---|---|
| T38 | **P0** — Cargo workspace scaffold + `myna-core` crate: `AudioFormat`/`PcmChunk` (per `docs/audio-adapter-api.md` §2), event/session/protocol types + serde wire codec mirroring `myna.core`, `PROTOCOL_VERSION="1"` | done | Charles | — | **Done 2026-07-01.** `rust/` workspace (`myna-core` + `myna-orchestrator`/`myna-cli` stubs); tokio not yet pulled (T39). `myna-core` covers audio/events/session/protocol/control with a JSON codec verified against **golden frames captured from Python `myna.core`** (compared as parsed JSON values — key order/whitespace differ, structure matches; both ends parse JSON). 25 unit tests green, clippy clean. Includes the new `ready` phase (T42) |
| T39 | **P1** — `BackendClient` trait + WS-over-UDS impl speaking the **existing** `myna.core` wire (`session.start`→binary PCM→`session.finish`; parse `session.created` + `transcription.*`) | done | Charles | T38, T14a | **Done 2026-07-01.** `myna-orchestrator::backend`: `BackendClient` trait → `BackendHandle` split into a cheap-clone `BackendSink` (audio/finish/abort up) + `BackendEvents` receiver (down), decoupling the FSM from the wire via channels so it can push audio and read events concurrently. `WsUnixBackend` (tokio + tokio-tungstenite over `UnixStream`) does the handshake (declares `protocol_version`, awaits `session.created`, maps a rejection error to `BackendError::Rejected`), then a pump task bridges the split WS to the channels (terminal-event/close/abort teardown; commit-drain: sink-drop keeps reading). Integration test spawns the real `myna-server --adapter fake` on a UDS and asserts the full scripted session round-trips (26 tests green, clippy clean). Also **added `--adapter fake` to the server CLI** (serves the permanent fixture over the wire, zero deps — reused by T42). Test launches the venv server binary **directly** (not `uv run`, which orphans a grandchild) so cleanup is reliable |
| T40 | **P2** — the FSM (centerpiece): two orthogonal regions + accept-gate, commit-drain, pre-ready drop, terminal/recoverable error mapping (enum; taxonomy provisional pending T31). Fake-backend fixture emitting the full `STATUS{loading→ready→transcribing}` + edge cases | done | Charles | T38 | **Done 2026-07-01.** Split into a **pure synchronous FSM** (`myna-orchestrator::fsm`) — `(state, input) -> (state', actions)`, no I/O — and a thin async **driver** (`::driver::run_session`) that only pumps client inputs + backend events through it. Two orthogonal regions: session (`Active→Finalizing→Done`/`Failed`/`Aborted`) × residency (`Unloaded→Loading→Resident`), with the accept-gate (`Active ∧ Resident`), commit-drain (`Finalizing` is non-terminal; only `done` completes), pre-ready drop (`AudioDropped{NotResident}`), residency **re-lapse** (idle-unload mid-session re-closes the gate — open item #4), and a provisional `ErrorDisposition` enum (`Terminal`/`Recoverable`/`Advisory`) via `classify_error` (only `not_ready`→advisory + `overloaded`→recoverable mapped; rest terminal, pending T31). Fixture: `backend::fake::FakeBackend` (Rust mirror of the Python `FakeAdapter`, permanent regression asset) — scripted, model-free, emits the full `STATUS` liveness + `happy_path`/`commit_drain`/`mid_stream_error`/`slow_ready` scripts; `WaitForFinish` step stages §3C precisely. **Edge cases 3A/3B/3C each have deterministic pure-FSM tests** (17 FSM tests) + 4 async driver tests over the fixture; clippy clean. Wire-agnostic — the FSM never touches a socket, so IE115 names (T43) are a backend swap, not a rebuild |
| T41 | **P3** — boundary mocks + demo bin: `AudioSource` trait + `WavFileSource` (real-time paced, `corpus/real`), `Trigger` (stdin), `TextSink` (stdout); wired into the `myna-cli` orchestrator binary end-to-end against the real Python server | done | Charles | T39, T40, T20 | **Done 2026-07-01.** Three boundary traits, each with a mock, matching the audio-adapter API so real impls drop in: `audio::AudioSource` (§3 signature: `format()` + `capture()→Stream`, `CaptureError` §4) + `WavFileSource` (minimal RIFF parser, real-time pacing, graceful `StopHandle` per §5); `trigger::Trigger` (+`StdinTrigger` two-Enter flow / `ScriptedTrigger`) for the T21 hotkey; `sink::TextSink` (+`StdoutSink` / `CollectingSink`) for the T22 injector. `runner::run_dictation` composes source→FSM driver→sink for one utterance (clean end→`EndOfAudio`/finalize, fault→`Abort`; negotiates the source format into the config). `myna-dictate` binary wires them for push-to-talk (`--socket`/`--clip`/`--corpus`), Release→graceful-stop via `select!`. The runner **gates capture on the first `ready`** (holds the pump until residency, `notify_one`-based, aborts on session end so a load-time error can't hang) — so a paced/replayable source can't drain into a closed accept-gate during a slow cold load (the client half of ie115-lifecycle §3A; pairs with T42's adapter-emits-`ready`). **Verified end-to-end against a live Python `myna-server`** — `--adapter fake` (`tests/dictation_e2e.rs`, skip-friendly like T39) *and* `--adapter whisper --model base` manually (exact real-speech transcript, zero dropped audio). 29 orchestrator tests + clippy clean. Independent of T20 (Matias's PipeWire crate replaces `WavFileSource` behind the same trait) |
| T42 | Adapters emit `ready` after load, before consuming audio, so the residency accept-gate opens over the wire (fake **and** every real adapter) | done | Charles | T02 | **Done 2026-07-01 — was a correctness bug, not just mock fidelity.** Manual `myna-dictate`→whisper runs surfaced a **deadlock**: the whisper/nemotron/qwen adapters emitted `preparing` heartbeats during load but only emitted `transcribing`/`final` *after* receiving audio, while the client's accept-gate (T40) drops audio until it sees `ready` — so no audio ever flowed and the session finalized empty. Per *fix-the-adapter-not-the-harness*: all three real adapters now `emit(TranscriptionProgress(phase=PHASE_READY))` right after `_load_model_with_heartbeat`, before the `async for chunk in audio` loop; the fake adapter's `default_script` opens with `preparing→ready` too. Also re-exported `PHASE_READY` from `myna.core` (was defined in `events` but not surfaced). Vocab (the `ready` phase) had already landed. Second half in T41: the runner now **holds capture until the first `ready`** so a slow cold load can't drain a paced source into the closed gate. Regression: `fake.rs::runner_gates_all_audio_until_ready` (probe backend asserting no audio precedes `ready`); verified live end-to-end against `whisper --model base` (exact transcript, zero drops). 103 py + 28 orch tests green |
| T43 | **P4** — IE115 `BackendClient` (Rust): a second backend speaking IE115 names (`session.update`/`input_audio_buffer.append`/`conversation.item.input_audio_transcription.*`/`STATUS`) behind the wire-agnostic FSM; `--dialect ie115` on `myna-dictate` | done | Charles | T40, T45 | **Done 2026-07-02.** `backend::ws_unix_ie115::WsUnixIe115Backend` — a second `BackendClient` behind the *unchanged* FSM (the trait boundary held: only the backend swapped). Sends `session.update` eagerly, PCM as binary by default / base64 `append` with `.base64_audio(true)`, `input_audio_buffer.commit` on finish; `Decoder` maps STATUS→progress, `completed`→final, `error`→terminal, synthesises `done` on close. `--dialect internal\|ie115` + `--base64-audio` on `myna-dictate` (generic run loop over both backends). 5 unit tests + workspace clippy clean; verified against live whisper/nemotron/qwen-c (see T46) |

---

T33 (audio sample-encoding) is the audio half of this reconciliation — IE115's
fixed 24 kHz mandate makes the "negotiate format via `input_formats`, don't
hardcode" position concrete. T31 (error-code taxonomy) is unchanged by IE115:
IE115's `error` has `type`/`code`/`message` but, like IE114, defines no code
set — same gap.

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
6. **M5 — Orchestrator loop (Rust):** T38–T42. Trigger → WAV → transcription
   through the wire-agnostic FSM against the real Python server; IE115 wire (T43)
   follows once Ivano's server lands.
