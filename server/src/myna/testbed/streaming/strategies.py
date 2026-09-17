# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (C) 2026 Canonical Ltd.
#
# _AdaptiveVad and the SilenceCut chunking policy below are ported from
# Murmure <https://github.com/Kieirra/murmure> (src-tauri audio/vad.rs and
# audio/chunking.rs), Copyright (C) 2025-2026 Kieirra, licensed under the GNU
# Affero General Public License version 3. Modified by Canonical Ltd. in 2026:
# ported from Rust to Python and fitted to the streaming strategy seam; see the
# git history for the changes. The rest of this file is Canonical's and is
# offered under AGPL-3.0-or-later; the file as a whole is AGPL-3.0-only.
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
import math
from dataclasses import dataclass, field

import numpy as np
from numpy.typing import NDArray

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
_FRAME_LEN = max(1, int(SC_FRAME_S * 16_000))

# A noise floor tracked from the signal alone drifts up into continuous
# speech. With no real silence to pull it down it converges on the quiet
# frames *inside* speech, and the silence threshold derived from it lands
# mid-speech: measured 2026-09-17 over the 261 s no-gaps stress clip, the
# floor settles at 0.12x the speech level, 63% of frames read "silent" and a
# false pause cut fires every ~30 s. Speech is the only reference the signal
# offers, so the floor is capped at a fraction of a peak-held speech level:
# usable dictation audio sits well above its own noise floor, and capping too
# hard only costs pause cuts (the force cut still bounds the window, with the
# overlap that makes it safe) while capping too little deletes words.
# 0.05 keeps every genuine pause of the 302 s long-form clip and drops its
# drift-driven ones; 0.03 also loses genuine pauses.
_SPEECH_FLOOR_RATIO = 0.05
_SPEECH_DECAY = 0.9995  # per frame, ~20 min to fall a decade: recent speech
# A pause cut may retire audio without overlap, which is only safe if no word
# straddles it. Measured on the same clips: the loudest raw frame in a genuine
# pause reaches 0.13-0.21x the speech level, a drifted false pause 0.29-0.88x.
_CUT_QUIET_RATIO = 0.25

# Scripts written without spaces between words (CJK ideographs, kana,
# fullwidth forms): a decoder that cannot segment words emits one token per
# character, and region joins add no space beside them.
UNSPACED_CHARS = "\u3000-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uff00-\uffef"


@dataclass(frozen=True)
class Word:
    text: str
    start: float  # absolute session seconds
    end: float


@dataclass
class Hypothesis:
    """One decode of the uncommitted window, absolute session seconds."""

    words: list[Word] = field(default_factory=list)


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
    ) -> CommitDecision | None:
        if not current.words:
            return None
        agreed_end = 0.0
        agreed_count = 0
        if last is not None and last.words:
            prev_words = [w.text.strip().lower() for w in last.words]
            curr_words = [w.text.strip().lower() for w in current.words]
            matcher = difflib.SequenceMatcher(a=prev_words, b=curr_words, autojunk=False)
            for tag, i1, i2, j1, _j2 in matcher.get_opcodes():
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
        if not agreed_count:
            return None
        return CommitDecision(agreed_end, tuple(current.words[:agreed_count]))

    def boundary_commit(
        self, current: Hypothesis, cut: float, retain_from: float
    ) -> CommitDecision | None:
        return boundary_commit(current, cut, retain_from)


def boundary_commit(current: Hypothesis, cut: float, retain_from: float) -> CommitDecision | None:
    """What to commit from a final decode of the window up to a forced
    ``cut``, where audio before ``retain_from`` is retired. Only words
    that lie wholly in the retained overlap and end within TAIL_GUARD_S of
    the cut are held back for the next window; every other word commits
    now, because its audio does not survive the cut. A decoder that cannot
    time its words should stamp each with the span of its whole input, so
    nothing is held back."""
    count = 0
    for w in current.words:
        if w.start >= retain_from and w.end > cut - TAIL_GUARD_S:
            break
        count += 1
    if not count:
        return None
    return CommitDecision(current.words[count - 1].end, tuple(current.words[:count]))


class _AdaptiveVad:
    """Port of murmure's AdaptiveVad (audio/vad.rs): a noise floor tracked
    with asymmetric EMAs, a speech threshold at floor*5 (clamped
    [0.004, 0.08]) and a silence threshold at floor*3; per-frame RMS smoothed
    with EMA alpha 0.3. `update` returns "not-started" until speech has been
    seen once, then "active"/"silent".

    Modified: once speech has been heard the floor is capped at
    [`_SPEECH_FLOOR_RATIO`] of ``speech_level``, a peak-held envelope of the
    smoothed signal, so it cannot drift up into continuous speech. Before the
    first speech frame the signal *is* the ambient, and the murmure tracker
    runs unmodified."""

    def __init__(self) -> None:
        self._floor = 0.003
        self._smoothed = 0.0
        self._started = False
        self.speech_level = 0.0

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
        self.speech_level = max(self._smoothed, self.speech_level * _SPEECH_DECAY)
        self._floor = min(self._floor, self.speech_level * _SPEECH_FLOOR_RATIO)
        silence_threshold = min(max(self._floor * 3.0, 0.004 * 0.6), 0.08 * 0.6)
        return "silent" if self._smoothed < silence_threshold else "active"


@dataclass(frozen=True)
class Cut:
    """A cut the chunking policy decided on.

    ``at`` is absolute audio seconds and always an exact sample position.
    ``forced`` says the window reached its force length rather than pausing.
    ``silent`` says the audio at the cut was verified quiet against the
    tracked speech level - only then can the region retire without overlap,
    because only then can no word straddle the cut."""

    at: float
    forced: bool
    silent: bool


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
        self._run_peak = 0.0  # loudest raw frame of the current silence run
        self._heard_since_cut = False
        self._scanned = 0  # absolute samples; audio before this was VAD-fed

    @property
    def force_cut_seconds(self) -> float:
        return self._force_cut

    @property
    def heard_since_cut(self) -> bool:
        """Whether the VAD judged any frame since the last cut active."""
        return self._heard_since_cut

    def mark_cut(self, at: float) -> None:
        """The window was cut at ``at``: restart the silence run there."""
        self._silence_run = 0.0
        self._run_peak = 0.0
        self._heard_since_cut = False
        self._scanned = round(at * 16_000)

    def unscanned_offset(self, window_start: float) -> int:
        """Samples into the window where the next `observe` starts reading.

        Frames tile forward from the scan position in exact samples, so each
        sample is fed to the VAD exactly once. Float seconds in, integer
        samples out: a window position is a whole sample by construction."""
        start = round(window_start * 16_000)
        return max(self._scanned, start) - start

    def observe(
        self,
        samples: NDArray[np.float32],
        window_start: float,
        window_end: float,
        *,
        offset: int = 0,
    ) -> Cut | None:
        """Return a `Cut` if the window should be committed now.

        ``samples`` are the window from ``offset`` samples in, which must not
        exceed `unscanned_offset`: only audio not yet scanned is read."""
        start = round(window_start * 16_000)
        end = round(window_end * 16_000)
        if end - start >= round(self._force_cut * 16_000):
            self.mark_cut(window_end)
            return Cut(window_end, forced=True, silent=False)  # decode [frontier, cut) once
        # Feed only the new audio (in SC_FRAME_S frames, murmure-tick parity).
        off = self.unscanned_offset(window_start)
        if offset > off:
            raise ValueError("observe needs the samples from unscanned_offset() onwards")
        frame_len = _FRAME_LEN
        while off + frame_len <= offset + len(samples):
            frame = samples[off - offset : off - offset + frame_len]
            rms = math.sqrt(float(np.mean(frame * frame)))
            activity = self._vad.update(rms)
            off += frame_len
            self._scanned = start + off
            if activity == "active":
                self._heard_since_cut = True
            # Arm per frame (murmure arms when the buffer *reaches* SC_ARM_S):
            # only frames ending past the arm point accumulate silence.
            if off < round(self._arm * 16_000):  # off is the frame end in the window
                continue
            if activity == "silent":
                self._silence_run += SC_FRAME_S
                self._run_peak = max(self._run_peak, rms)
                if self._silence_run >= self._silence_cut:
                    # murmure cuts the tick the run crosses — at this
                    # frame, not the next call boundary: a pause that ends
                    # mid-call would otherwise go active and reset the run
                    # before the check ever saw it (missed 1.1 s pause on
                    # stream-2277-02, 2026-07-29). The cut covers audio up
                    # to this frame — the trailing silence rides in, so no
                    # word straddles.
                    quiet = self._run_peak <= self._vad.speech_level * _CUT_QUIET_RATIO
                    at = (start + off) / 16_000
                    self.mark_cut(at)
                    return Cut(at, forced=False, silent=quiet)
            elif activity == "active":
                self._silence_run = 0.0
                self._run_peak = 0.0
        return None
