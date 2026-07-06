# Streaming transcription (T08) — the revision contract

**Date:** 2026-07-03
**Status:** Draft for ratification — pins the one decision that gates T08 (how
pre-commit text may change) and the new testbed metric that makes streaming
quality measurable. Written to be decided the way `ie115-wire.md` was: small,
concrete, demoable.
**Authors:** Claude, with Charles
**Sources:** `docs/project-plan.md` (T08, T36, M2), `src/myna/core/events.py`
(the flat vocab + IE115 mapping), `src/myna/core/wire_ie115.py` (delta hooks
already in place), `src/myna/testbed/harness.py` (`Metrics`),
`docs/architecture/ie115-wire.md §7.4`.

## 0. What this note decides

Every adapter today is **commit-on-finalize**: it emits one `transcription.final`
(and `done`) when audio ends, nothing committed before. T08 is the first time we
emit committed text *during* an utterance. The transport already streams (PCM
frames in, events out); what's missing is a **contract for text that appears
before the segment is closed** — specifically, *may it change after it is shown?*

That single question decides whether T08 is a per-adapter change behind existing
hooks or a protocol change that reintroduces retraction. Everything else
(segmentation, which model, latency dials) follows from it. This note recommends
an answer and defines the metric to hold it to.

## 1. What already exists (so this is scoped, not greenfield)

- `TranscriptionProgress.snippet` — unstable liveness text, "no accuracy
  guarantee, no retraction — UI animation only." Emitted by no adapter today.
- `wire_ie115.py` (as amended 2026-07-06, T47 — persistent connections): the
  codec already speaks committed deltas both ways — `transcription.final` ↔
  IE115 `…transcription.delta` (committed, append-only), `done` ↔ `…completed`
  (per-commit terminal). Contract (a) below is therefore already the wire's
  shape; unstable `snippet` rides `STATUS`, never `delta`.
- T36 fixed the semantics boundary: IE115 `delta` = committed incremental text;
  our `snippet` = unstable liveness. Kept distinct on purpose.
- `Metrics.time_to_first_snippet` already measures first-unstable-text latency.

So the hooks are placed. T08 fills them in — once §2's contract is chosen.

## 2. "Streaming" is two separable things

1. **Earlier commit (segmentation).** The model closes segments *during* the
   utterance, so `final` fires mid-stream instead of once at end-of-audio. This
   is about *when* committed text lands and how the utterance is cut into
   segments. It needs no new event type — just more, earlier `final`s.
2. **Pre-commit text (deltas).** Surfacing not-yet-committed text so the UI can
   animate ahead of the commit frontier. This is the IE115 `delta` slot and the
   thing that carries the revision question.

(1) is safe and model-driven; (2) is where the design risk lives. They can ship
independently — and (1) alone already improves perceived latency.

## 3. The revision contract — recommendation: **append-only committed**

The team dropped `partial`/`replace`/epoch retraction as confusing. Two models
survive that decision:

- **(a) Append-only committed deltas** (IE115 style). Text emitted early is text
  we never take back; a delta *extends* the committed prefix, `completed`/`final`
  just closes the segment. No retraction anywhere on the wire.
- **(b) Revisable partials** (LocalAgreement). Pre-commit text churns and is
  overwritten until it stabilises. This re-introduces retraction, scoped to
  non-final text — exactly the confusion we removed, back through a side door.

**Recommendation: (a).** Reasons:

- It matches IE115 (`delta` is committed-incremental) and needs no new
  retraction vocabulary — `final` stays "never retracted", deltas are just an
  early, finer-grained `final`.
- **Nemotron** (transducer) has a monotonic commit frontier that *is* append-only
  — the plan already notes it "matches our `final` contract." (a) is free there.
- It keeps the client trivial: append text, never rewind. No epoch/replace state
  machine in the Rust FSM or IBus injection path (T22), where retraction would
  mean deleting already-typed characters — a genuine hazard.

The cost lands on **AED re-decode (whisper)**: it cannot honestly emit
append-only fine-grained text, because its hypothesis churns. Under (a), whisper
streaming emits **coarser, later segments** (only text a LocalAgreement-style
window has stabilised) rather than churning partials. We accept worse
whisper streaming latency in exchange for one clean contract everywhere. If a UI
still wants motion, it uses the *unstable* `snippet` channel — explicitly
non-committed, already in the vocab — never promoted to committed text.

Net rule:

> Committed text (delta or final) is append-only and never retracted. Unstable
> `snippet` may change arbitrarily and must never be shown as committed.

## 4. Wire & event shape

- Internal: reuse `transcription.final` for each committed segment; a streaming
  adapter simply emits several, earlier. **No new committed-delta event for the
  PoC** — segmentation (§2.1) covers the demoable win and needs no version churn.
- IE115 (as amended 2026-07-06, T47): a committed segment maps to
  `…transcription.delta` and the utterance terminal to `…transcription.completed`
  — the codec is already symmetric both ways, so segmentation-only streaming
  needs **no** wire change on either dialect.
- Unstable motion stays on `progress.snippet` ↔ (optionally) `STATUS`; never
  `delta`.

Adding a *committed-delta* event type is a **`PROTOCOL_VERSION` bump** (new event
in the contract). Emitting more `final`s is **not** — it's the same vocabulary,
so segmentation-only streaming ships without a bump.

## 5. Per-adapter behaviour under (a)

| Adapter | Frontier | Streaming shape |
|---|---|---|
| Nemotron (transducer) | native, monotonic | append-only committed segments, frame-cheap; `att_context_size` is the latency dial |
| Whisper (AED re-decode) | none native | LocalAgreement window → coarser/later committed segments; churn hidden behind `snippet` |
| Qwen-c (LLM decoder) | monotonic commit frontier when revisited | append-only; sub-realtime on weak CPUs (see T10a), so streaming is a follow-up, not MVP |

## 6. New testbed metric — streaming quality

Batch WER can't score streaming; that's why T08 lives in M2. `finalize_latency`
today measures only end-of-audio → terminal. Streaming needs two additions to
`harness.Metrics`, computed from the timed event stream:

- **`time_to_first_committed`** — session-open (or first-audio) → first
  `transcription.final`. Distinct from `time_to_first_snippet` (that's *unstable*
  text). This is "how soon does trustworthy text appear."
- **`commit_stability`** — a guard on contract (a): since committed text must be
  append-only, this counts any committed prefix that is *not* a prefix-extension
  of the previous committed text (i.e. a retraction). Under (a) it must be **0**;
  a non-zero value is a contract violation, not a quality knob. (For a hypothetical
  (b) adapter it would degrade to a churn rate — normalised edit distance between
  successive committed states.)

WER stays scored on the final committed transcript (unchanged). The stability
metric polices the *path* to that transcript; the latency metric prices it.

This is the payoff of the multi-model harness: (a)-vs-(b), transducer-vs-AED
partial behaviour, and the `att_context_size` latency dial all become
measurable side-by-side, over the same real corpus, in `dev/matrix.py` once a
streaming axis is added to the sweep (the T11 note already flags this).

## 7. Scope & sequencing

1. **Ratify §3** (append-only committed). One decision; unblocks the rest.
2. Add `time_to_first_committed` + `commit_stability` to `Metrics` (testbed-only,
   no wire change) — so we can measure before we build.
3. **Segmentation-only streaming** on nemotron first (native frontier, no bump):
   emit earlier `final`s; verify `commit_stability == 0` and improved
   `time_to_first_committed` over the real corpus.
4. Whisper LocalAgreement segmentation under the same contract (accept coarser
   segments); confirm no retraction leaks past `snippet`.
5. *Only if wanted:* sub-segment committed deltas → IE115 `delta` encoder path +
   `PROTOCOL_VERSION` bump. Defer until a consumer needs finer granularity than
   segment-level `final`.

Steps 1–4 need no protocol bump and are demoable on the existing loopback.

## 8. Open questions

1. **Segment boundary policy** — silence-based, fixed-window, or model-native?
   Affects `time_to_first_committed` and segment WER. Needs T12 lab runs.
2. **Whisper LocalAgreement window/latency** — how coarse is acceptable before
   whisper streaming is just commit-on-finalize with extra steps?
3. **`snippet` in dictation** — does IBus injection (T22) ever show unstable
   text, or is `snippet` UI-only for a future GUI? If never injected, we could
   drop it from the dictation path entirely and keep it for capability demos.
4. **Barge-in / endpointing** — out of scope here, but streaming makes it
   relevant; note for a later workstream.
