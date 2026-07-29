"""Commit strategy for streaming re-decode (feature 008).

The seam (research.md Decision 2): the whisper adapter's loop re-decodes the
uncommitted window on a cadence; the strategy only decides *what to commit
when*. The strategy is wire-invisible — everything it emits travels as the
007 committed/unstable dispositions (contracts/emission-semantics.md).

2026-07-28 strategy triage: the 008 sweep compared three strategies on the
26-28 s concatenated streams (results/streaming-watermarks.json).
local-agreement was the only one to pass SC-001 (ttfc 2.4-3.5 s vs
tail-mutation's 6.8-7.8 s; fixed-head emitted no unstable and committed at
~18 s) at equal WER, with the strongest right-context guarantee of the
re-decode pair and no whisper-segment-specific dependencies. tail-mutation
and fixed-head were removed; fixed-head's decode-once == batch WER control
result (the +2.4 pp re-decode gap is right-context loss, not plumbing)
stands in the watermark record. If a tier where re-decode is unaffordable
ever appears, batch mode is the floor and a chunked strategy can be revived
from git history.

All times on hypotheses/decisions are absolute session seconds so comparisons
survive frontier advancement (the window origin moves as commits land).
"""

from __future__ import annotations

import difflib
from dataclasses import dataclass, field

# Words ending within this of the window tail have insufficient right context
# to commit (whisper boundary heuristic, contracts/emission-semantics.md).
TAIL_GUARD_S = 0.5
# Max timestamp drift for two passes to count as agreeing.
AGREE_DRIFT_S = 0.3


@dataclass(frozen=True)
class Word:
    text: str
    start: float  # absolute session seconds
    end: float


@dataclass
class Hypothesis:
    """One decode of the uncommitted window, absolute session seconds."""

    words: list[Word] = field(default_factory=list)

    @property
    def text(self) -> str:
        return "".join(w.text for w in self.words).strip()


@dataclass(frozen=True)
class CommitDecision:
    """What to commit for one tick; ``None`` from `commit_rule` ⇒ nothing.

    ``commit_end`` is the absolute audio time the commit covers (the window
    frontier advances to it). ``commit_words`` are the words behind the
    commit (absolute times) — the loop filters out anything a previous
    commit already emitted (overlap dedupe, I2) and emits the remainder.
    """

    commit_end: float
    commit_words: tuple[Word, ...]


class LocalAgreement:
    """Commit the longest prefix of the current hypothesis whose words agree
    with the previous pass (text match via alignment, timestamp drift within
    AGREE_DRIFT_S). The agreed prefix never ends within TAIL_GUARD_S of the
    window tail (insufficient right context). Unstable display text is the
    loop's business: the uncommitted remainder of the hypothesis (I3)."""

    def commit_rule(
        self,
        last: Hypothesis | None,
        current: Hypothesis,
        window_end: float,
        force: bool,
    ) -> CommitDecision | None:
        if not current.words:
            return None
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
        if not agreed_count:
            return None
        return CommitDecision(agreed_end, tuple(current.words[:agreed_count]))
