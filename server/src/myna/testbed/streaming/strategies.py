"""Commit strategies for streaming re-decode (feature 008).

The seam (research.md Decision 2): the whisper adapter's loop re-decodes the
uncommitted window on a cadence; a strategy only decides *what to commit
when*. Strategies are wire-invisible — everything they emit travels as the
007 committed/unstable dispositions (contracts/emission-semantics.md).

Two shapes:

- **re-decode strategies** (``tail-mutation``, ``local-agreement``): every tick
  the loop decodes the whole uncommitted window; the strategy compares the new
  hypothesis against the last one and returns a commit decision.
- **chunked strategies** (``fixed-head``): the loop watches the audio for a
  silence cut; only then is the region up to the cut decoded *once* and
  committed wholesale (murmure-style — no re-decode, cheapest compute).

All times on hypotheses/decisions are absolute session seconds so comparisons
survive frontier advancement (the window origin moves as commits land).
"""

from __future__ import annotations

import difflib
from dataclasses import dataclass, field
from typing import Protocol

import numpy as np

# Words ending within this of the window tail have insufficient right context
# to commit (whisper boundary heuristic, contracts/emission-semantics.md).
TAIL_GUARD_S = 0.5
# LocalAgreement: max timestamp drift for two passes to count as agreeing.
AGREE_DRIFT_S = 0.3
# Tail-mutation: a trailing segment repeated unchanged this many times is
# force-committed (stuck-partial escape, WhisperLive parity).
STUCK_PASSES = 10
# Fixed-head segmentation starting constants (murmure-informed; re-validate
# on the real corpus before ratifying watermarks).
FH_ARM_S = 15.0            # once the window is this long, silence cuts it
FH_SILENCE_CUT_S = 0.5     # silence run that cuts an armed window
FH_FORCE_CUT_S = 60.0      # hard cut when no silence was found
FH_MIN_RMS = 0.004         # clamp for the adaptive silence threshold
FH_RMS_FRAME_S = 0.03      # energy analysis frame


@dataclass(frozen=True)
class Word:
    text: str
    start: float  # absolute session seconds
    end: float


@dataclass(frozen=True)
class SegmentText:
    text: str
    start: float  # absolute session seconds
    end: float
    no_speech_prob: float = 0.0


@dataclass
class Hypothesis:
    """One decode of the uncommitted window, absolute session seconds."""

    words: list[Word] = field(default_factory=list)
    segments: list[SegmentText] = field(default_factory=list)

    @property
    def text(self) -> str:
        return "".join(w.text for w in self.words).strip()


@dataclass(frozen=True)
class CommitDecision:
    """What to emit for one tick.

    ``commit_text`` empty ⇒ nothing committed this tick. ``commit_end`` is the
    absolute audio time the commit covers (frontier advances to it).
    ``commit_words`` are the words behind ``commit_text`` (absolute times) —
    the loop filters out anything already covered by a previous commit
    (overlap dedupe, I2). ``unstable_text`` is the display-only remainder
    (may be empty — fixed-head emits no unstable by design).
    """

    commit_text: str = ""
    commit_end: float = 0.0
    unstable_text: str = ""
    commit_words: tuple[Word, ...] = ()


class StreamingStrategy(Protocol):
    name: str
    mode: str  # "redecode" | "chunked"


class RedecodeStrategy(Protocol):
    name: str
    mode: str

    def commit_rule(
        self,
        last: Hypothesis | None,
        current: Hypothesis,
        window_end: float,
        force: bool,
    ) -> CommitDecision: ...


class ChunkedStrategy(Protocol):
    name: str
    mode: str

    def observe(self, samples: np.ndarray, window_start: float, window_end: float) -> float | None:
        """Return an absolute cut time if the window should be committed now."""


def _join(words: list[Word]) -> str:
    return "".join(w.text for w in words).strip()


class TailMutation:
    """Commit all complete segments except the trailing one; the trailing
    segment is the unstable remainder (revised wholesale between passes —
    legal under I3). Subsumes the WhisperLive commit heuristic in-adapter
    (research.md Decision 4); weakest right-context guarantee of the three —
    commit stability is measured, not assumed."""

    name = "tail-mutation"
    mode = "redecode"

    def __init__(self) -> None:
        self._last_unstable = ""
        self._stuck = 0

    def commit_rule(
        self,
        last: Hypothesis | None,
        current: Hypothesis,
        window_end: float,
        force: bool,
    ) -> CommitDecision:
        # Over cap with no usable segment structure: the window must shrink
        # (I6) — commit every word except the tail guard.
        segs = [s for s in current.segments if s.no_speech_prob <= 0.6 and s.text.strip()]
        if not segs:
            if force and current.words:
                forced = [w for w in current.words if w.end <= window_end - TAIL_GUARD_S]
                if forced:
                    return CommitDecision(
                        _join(forced),
                        forced[-1].end,
                        _join([w for w in current.words if w.end > forced[-1].end]),
                        tuple(forced),
                    )
            return CommitDecision()
        complete, trailing = segs[:-1], segs[-1]
        commit_text = " ".join(s.text.strip() for s in complete).strip()
        commit_end = complete[-1].end if complete else 0.0
        unstable = trailing.text.strip()

        # Stuck-partial escape: unchanged trailing segment across STUCK_PASSES
        # consecutive ticks ⇒ force-commit it and advance past its end.
        if unstable and unstable == self._last_unstable:
            self._stuck += 1
        else:
            self._stuck = 0
        self._last_unstable = unstable
        if self._stuck > STUCK_PASSES:
            commit_text = " ".join(
                x for x in (commit_text, unstable) if x
            ).strip()
            commit_end = trailing.end
            unstable = ""
            self._stuck = 0

        # Over cap: the window must shrink (I6) — commit everything except the
        # tail guard, even without a segment boundary.
        if force and commit_end < window_end - TAIL_GUARD_S:
            forced = [w for w in current.words if w.end <= window_end - TAIL_GUARD_S]
            if forced:
                commit_text = _join(forced)
                commit_end = forced[-1].end
                unstable = _join([w for w in current.words if w.end > commit_end])

        commit_words = tuple(w for w in current.words if w.end <= commit_end)
        return CommitDecision(commit_text, commit_end, unstable, commit_words)


class LocalAgreement:
    """Commit the longest prefix of the current hypothesis whose words agree
    with the previous pass (text match via alignment, timestamp drift within
    AGREE_DRIFT_S). The agreed prefix never ends within TAIL_GUARD_S of the
    window tail (insufficient right context). Unstable = the un-agreed
    remainder of the current hypothesis. Default strategy (Spike S1 gate)."""

    name = "local-agreement"
    mode = "redecode"

    def commit_rule(
        self,
        last: Hypothesis | None,
        current: Hypothesis,
        window_end: float,
        force: bool,
    ) -> CommitDecision:
        if not current.words:
            return CommitDecision()
        agreed_end = 0.0
        agreed_count = 0
        if last is not None and last.words:
            prev_words = [w.text.strip().lower() for w in last.words]
            curr_words = [w.text.strip().lower() for w in current.words]
            matcher = difflib.SequenceMatcher(a=prev_words, b=curr_words, autojunk=False)
            for tag, i1, i2, j1, j2 in matcher.get_opcodes():
                if tag != "equal":
                    break  # longest *prefix* agreement only
                for k in range(i2 - i1):
                    prev_w, curr_w = last.words[i1 + k], current.words[j1 + k]
                    if abs(prev_w.start - curr_w.start) > AGREE_DRIFT_S:
                        break
                    if curr_w.end > window_end - TAIL_GUARD_S:
                        break
                    agreed_count = j1 + k + 1
                    agreed_end = curr_w.end
        if force and agreed_end < window_end - TAIL_GUARD_S:
            forced = [w for w in current.words if w.end <= window_end - TAIL_GUARD_S]
            if forced and (not agreed_count or forced[-1].end > agreed_end):
                agreed_count = len(forced)
                agreed_end = forced[-1].end
        committed = current.words[:agreed_count]
        remainder = current.words[agreed_count:]
        return CommitDecision(_join(committed), agreed_end, _join(remainder), tuple(committed))


class FixedHead:
    """Chunked commit (murmure-style): no re-decode. The loop feeds energy
    frames; once the window is armed (>= FH_ARM_S) a silence run of
    FH_SILENCE_CUT_S cuts it; a window reaching FH_FORCE_CUT_S is hard-cut.
    The region up to the cut is decoded once and committed wholesale, so
    right-context at commit time is a real pause — the strongest commit of
    the three, at the coarsest latency. Emits no unstable text by design."""

    name = "fixed-head"
    mode = "chunked"

    def __init__(self) -> None:
        self._floor = 0.003
        self._silence_run = 0.0

    def _update_floor(self, rms: float) -> None:
        if rms < self._floor:
            self._floor = 0.2 * rms + 0.8 * self._floor
        elif rms <= self._floor * 10:
            self._floor = 0.005 * rms + 0.995 * self._floor

    def observe(self, samples: np.ndarray, window_start: float, window_end: float) -> float | None:
        duration = window_end - window_start
        if duration >= FH_FORCE_CUT_S:
            return window_end  # loop decodes [frontier, cut) once
        if duration < FH_ARM_S:
            return None
        frame_len = max(1, int(FH_RMS_FRAME_S * 16_000))
        silence_threshold = max(FH_MIN_RMS, self._floor * 3.0)
        cut_at: float | None = None
        self._silence_run = 0.0
        for off in range(0, len(samples) - frame_len + 1, frame_len):
            frame = samples[off : off + frame_len]
            rms = float(np.sqrt(np.mean(frame * frame))) if len(frame) else 0.0
            self._update_floor(rms)
            t = window_start + (off + frame_len) / 16_000
            if rms < silence_threshold:
                self._silence_run += FH_RMS_FRAME_S
                if self._silence_run >= FH_SILENCE_CUT_S and t >= window_start + FH_ARM_S:
                    cut_at = t
                    # keep scanning: cut at the *latest* usable pause
            else:
                self._silence_run = 0.0
        return cut_at


_STRATEGIES: dict[str, type] = {
    TailMutation.name: TailMutation,
    LocalAgreement.name: LocalAgreement,
    FixedHead.name: FixedHead,
}


def make_strategy(name: str) -> StreamingStrategy:
    try:
        return _STRATEGIES[name]()
    except KeyError:
        raise ValueError(
            f"unknown streaming strategy {name!r} (have: {', '.join(sorted(_STRATEGIES))})"
        ) from None
