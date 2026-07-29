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

import numpy as np

# Words ending within this of the window tail have insufficient right context
# to commit (whisper boundary heuristic, contracts/emission-semantics.md).
TAIL_GUARD_S = 0.5
# Max timestamp drift for two passes to count as agreeing.
AGREE_DRIFT_S = 0.3

# Chunked commit (murmure audio/chunking.rs + vad.rs ports): once the window
# is SC_ARM_S long a silence run of SC_SILENCE_CUT_S cuts it; a window reaching
# SC_FORCE_CUT_S is hard-cut (the RollingWindow's overlap carries the tail, so
# a word straddling the forced cut is deduped at merge). Constants are
# murmure's proven defaults; re-validate on the real corpus before ratifying
# watermarks (008 T026).
SC_ARM_S = 15.0
SC_SILENCE_CUT_S = 0.5
SC_FORCE_CUT_S = 60.0
SC_FRAME_S = 0.03  # VAD analysis frame (~murmure's 33 ms throttle tick)


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


class _AdaptiveVad:
    """Port of murmure's AdaptiveVad (audio/vad.rs): a noise floor tracked
    with asymmetric EMAs, a speech threshold at floor*5 (clamped
    [0.004, 0.08]) and a silence threshold at floor*3; per-frame RMS smoothed
    with EMA alpha 0.3. `update` returns "not-started" until speech has been
    seen once, then "active"/"silent"."""

    def __init__(self) -> None:
        self._floor = 0.003
        self._smoothed = 0.0
        self._started = False

    def update(self, rms: float) -> str:
        if rms < self._floor:
            self._floor = 0.2 * rms + 0.8 * self._floor
        else:
            floor_base = max(self._floor, 0.004 / 5.0)
            if rms <= floor_base * 10.0:
                self._floor = 0.005 * rms + 0.995 * self._floor
        self._smoothed = 0.3 * rms + 0.7 * self._smoothed
        speech_threshold = min(max(self._floor * 5.0, 0.004), 0.08)
        if self._smoothed > speech_threshold:
            self._started = True
        if not self._started:
            return "not-started"
        silence_threshold = min(max(self._floor * 3.0, 0.004 * 0.6), 0.08 * 0.6)
        return "silent" if self._smoothed < silence_threshold else "active"


class SilenceCut:
    """Chunked commit policy (murmure-style, no re-decode): the loop feeds the
    uncommitted window; once it is armed (>= SC_ARM_S) a silence run of
    SC_SILENCE_CUT_S cuts it at the window end (the trailing silence rides
    into the committed chunk, so no word straddles); a window reaching
    SC_FORCE_CUT_S is hard-cut. The region up to the cut is decoded once and
    committed wholesale — right-context at commit time is a real pause.
    Emits no unstable text by design (decode-once is the whole point).

    Parakeet TDT is the inhabitant (008 US3): its decode is chunk-final, so
    re-decode strategies buy nothing. State is incremental: only audio since
    the last observation is VAD-scanned; a cut resets the silence run but
    keeps the noise floor (murmure reset_silence_state parity).
    """

    mode = "chunked"

    def __init__(
        self,
        arm_seconds: float = SC_ARM_S,
        silence_cut_seconds: float = SC_SILENCE_CUT_S,
        force_cut_seconds: float = SC_FORCE_CUT_S,
    ) -> None:
        self._arm = arm_seconds
        self._silence_cut = silence_cut_seconds
        self._force_cut = force_cut_seconds
        self._vad = _AdaptiveVad()
        self._silence_run = 0.0
        self._scanned = 0.0  # absolute seconds; audio before this was VAD-fed

    def observe(self, samples: np.ndarray, window_start: float, window_end: float) -> float | None:
        """Return an absolute cut time if the window should be committed now."""
        duration = window_end - window_start
        if duration >= self._force_cut:
            self._silence_run = 0.0
            self._scanned = window_end
            return window_end  # loop decodes [frontier, cut) once
        # Feed only the new audio (in SC_FRAME_S frames, murmur-tick parity).
        # Frame phase is anchored at the window origin; a frame counts once its
        # end passes the previously scanned position.
        scan_from = max(self._scanned, window_start)
        frame_len = max(1, int(SC_FRAME_S * 16_000))
        off = int((scan_from - window_start) * 16_000) // frame_len * frame_len
        while off + frame_len <= len(samples):
            frame_end = window_start + (off + frame_len) / 16_000
            frame = samples[off : off + frame_len]
            rms = float(np.sqrt(np.mean(frame * frame)))
            activity = self._vad.update(rms)
            # Arm per frame (murmure arms when the buffer *reaches* SC_ARM_S):
            # only frames ending past the arm point accumulate silence.
            if frame_end > scan_from and frame_end - window_start >= self._arm:
                if activity == "silent":
                    self._silence_run += SC_FRAME_S
                elif activity == "active":
                    self._silence_run = 0.0
            off += frame_len
        self._scanned = window_end
        if duration >= self._arm and self._silence_run >= self._silence_cut:
            self._silence_run = 0.0
            return window_end
        return None
