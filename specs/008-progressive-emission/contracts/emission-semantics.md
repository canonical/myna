# Contract: Emission Semantics (server-side, per strategy)

**Feature**: `specs/008-progressive-emission`

The wire contract is 007's (`streaming-wire.md`) and does not change. This is
the server-side contract every emission strategy must satisfy.

## Invariants (all strategies, all backends)

- **I1 Append-only commit**: committed text is never retracted, rewritten, or
  re-emitted. `segment_index` is monotonic per utterance.
- **I2 Final equals concatenation**: the terminal transcript equals the
  verbatim concatenation of all committed segments. Deltas carry natural
  whitespace; only the first delta sheds its leading space.
- **I3 Unstable supersedes unstable**: an unstable delta replaces only the
  most recent unstable delta and never touches committed text. Unstable text
  is the uncommitted remainder of the current hypothesis.
- **I4 Commit clears unstable**: a committed delta invalidates outstanding
  unstable text.
- **I5 No unstable limbo**: end-of-audio commits or discards the uncommitted
  tail before the terminal event.
- **I6 Bounded memory**: the uncommitted audio window never exceeds
  `window_cap_seconds`; frontier advancement drops audio.
- **I7 Batch degenerate**: with streaming disabled, the service emits one
  committed segment at end-of-audio.

## Strategy commit rules

### local-agreement (whisper)

- Input: successive word-timestamped hypotheses of the uncommitted window.
- **Commit**: the longest prefix whose words agree with the previous
  hypothesis (text match, timestamp drift ≤ 0.3 s).
- **Unstable**: the remainder of the current hypothesis.
- Do not commit words ending within ~0.5 s of the window tail.

### chunked-commit (SilenceCut — parakeet)

The loop watches the uncommitted window with an adaptive-RMS VAD. Defaults:
15 s arm, 0.5 s silence cut, 60 s force cut, 1 s overlap; all three cut
thresholds are configurable. When a pause cuts, the region up to the cut is
decoded once and committed wholesale. No unstable text is emitted. The shared
loop enforces I1–I7.

### native (nemotron, sherpa)

The runtime emits per-step partials (→ unstable) and commits at natural
hypothesis or endpoint boundaries. The shared contract still applies, notably
I5 on end-of-audio.
