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
  equals the **verbatim** concatenation of all committed segments (no gaps,
  overlaps, or duplicates) — 007 FR-009. Committed deltas therefore carry
  their own natural whitespace (model word/segment texts keep their leading
  spaces; only the utterance's first delta sheds its leading space): a
  consumer that inserts each delta as it lands — with no separator logic —
  reproduces the final transcript exactly. (Pinned 2026-07-27 after the
  whisper loop stripped each delta, and injectors concatenating them verbatim
  produced "Thisis notworking that well.".)
- **I3 Unstable supersedes unstable**: an unstable delta replaces only the most
  recent unstable delta; it never touches committed text. Its display text is
  the *uncommitted remainder* of the current hypothesis — it never restates
  words a previous commit already emitted, and once text has been committed
  it keeps its natural leading space, so in-field preedit renders correctly
  as a continuation of the committed text.
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

### local-agreement (the shipped strategy)

- Input: successive word-timestamped hypotheses of the uncommitted window.
- **Commit**: the longest prefix of the current hypothesis whose words agree
  (text match, timestamp drift ≤ 0.3 s) with the previous hypothesis.
- **Unstable**: the remainder of the current hypothesis (expected to mutate).
- Never commit words ending within ~0.5 s of the window tail (insufficient
  right context — whisper boundary heuristic).

### Retired (2026-07-28 triage): tail-mutation, fixed-head

The long-stream watermark sweep (`results/streaming-watermarks.json`)
settled the strategy comparison: **local-agreement was the only SC-001
pass** (ttfc 2.4–3.5 s vs tail-mutation's 6.8–7.8 s; fixed-head ~18 s with
no unstable emission) at equal WER (LA/TM 7.19 %; FH == batch 4.79 %),
with the strongest right-context guarantee of the re-decode pair and no
whisper-segment-specific dependencies. tail-mutation (the WhisperLive
`completed` heuristic, weakest right-context guarantee — commit stability
was measured, not assumed) and fixed-head (decode-once-at-pause) were
removed from the tree. fixed-head's control result stands: decode-once ==
batch WER, so the +2.4 pp re-decode gap is right-context loss, not
plumbing. If a tier where re-decode is unaffordable ever appears, batch
mode is the floor and a chunked strategy can be revived from git history.

### native (nemotron, sherpa — informational)

Not a re-decode strategy: the runtime emits per-step partials (→ unstable) and
commits at its natural hypothesis/endpoint boundaries. Must still satisfy
I1–I7 (notably I5 on end-of-audio).

