# Contract: Emission Semantics (server-side, per strategy)

**Feature**: `specs/008-progressive-emission`

The wire contract is 007's (`streaming-wire.md`) and does not change. This
document is the **server-side contract every emission strategy must satisfy**,
expressed as testable invariants. It is what `dev/bench.py` sweeps assert
(SC-002, SC-003, SC-006), and what each strategy's `commit_rule` is specified
against.

## Invariants (all strategies, all backends)

- **I1 Append-only commit**: once any text is emitted with
  `disposition: committed`, that text is never retracted, rewritten, or
  re-emitted. `segment_index` is monotonic per utterance.
- **I2 Final equals concatenation**: the terminal event's full transcript
  equals the concatenation of all committed segments (no gaps, overlaps, or
  duplicates) — 007 FR-009.
- **I3 Unstable supersedes unstable**: an unstable delta replaces only the most
  recent unstable delta; it never touches committed text.
- **I4 Commit clears unstable**: a committed delta invalidates any outstanding
  unstable text (007 revision semantics).
- **I5 No unstable limbo**: end-of-audio resolves all outstanding text — the
  uncommitted tail is either committed or discarded (empty) before the
  terminal event.
- **I6 Bounded memory**: the uncommitted audio window never exceeds
  `window_cap_seconds`; frontier advancement drops audio (constitution V).
- **I7 Batch degenerate**: with streaming disabled, behavior is exactly 007
  batch: one committed segment at end (FR-009).

## Strategy commit rules

### local-agreement (default, gated on Spike S1)

- Input: successive word-timestamped hypotheses of the uncommitted window.
- **Commit**: the longest prefix of the current hypothesis whose words agree
  (text match, timestamp drift ≤ 0.3 s) with the previous hypothesis.
- **Unstable**: the remainder of the current hypothesis (expected to mutate).
- Never commit words ending within ~0.5 s of the window tail (insufficient
  right context — whisper boundary heuristic).
- Fallback if S1 no-go: agreement over segment-text prefixes instead of words.

### tail-mutation

- Input: one decode of the uncommitted window per tick.
- **Commit**: all complete segments except the trailing one (the WhisperLive
  `completed` heuristic, implemented in-adapter — this strategy subsumes that
  algorithm); a trailing segment repeated unchanged across > N passes (N ≈ 10)
  is force-committed (stuck-partial escape). Note: commits here have limited
  right context, so this is the weakest-guaranteed strategy — commit stability
  is measured (I1/I2 sweeps), not assumed.
- **Unstable**: the trailing segment, resent each pass (may be revised
  wholesale — legal under I3).

### fixed-head

- Input: VAD/energy segmentation of the incoming stream (arm 15 s, cut on
  500 ms silence, force-cut 60 s with 1 s overlap — starting constants,
  re-validated on our corpora).
- **Commit**: each finalized chunk, decoded once, immediately on cut. Overlap
  regions deduplicated at merge (word-level, ~6-word window).
- **Unstable**: optional per-chunk partial; MAY be omitted entirely (fixed-head
  is the strategy for tiers where re-decode is unaffordable — I1–I7 hold
  without unstable emission).

### native (nemotron, sherpa — informational)

Not a re-decode strategy: the runtime emits per-step partials (→ unstable) and
commits at its natural hypothesis/endpoint boundaries. Must still satisfy
I1–I7 (notably I5 on end-of-audio).

