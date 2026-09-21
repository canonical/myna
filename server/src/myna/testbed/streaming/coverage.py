"""Did the decoder transcribe the speech it was given?

Both CPU decoders in the testbed drop stretches of speech on particular inputs
- the Parakeet int8 encoder goes blank over part of a window (parakeet.py,
`_COLLAPSE_WORDS_PER_SECOND`), faster-whisper skips a sentence in a long region
- and both recover when the same audio is decoded again with its edges nudged.
Neither failure is visible in the text: the transcript reads cleanly, it is
just missing a sentence. What *is* visible is the audio the decode left
uncovered, which is what this module measures.

Measured 2026-09-17/18 over the 302 s long-form and 261 s no-gaps stress clips:
8.5% of 153 Parakeet regions and 1 of 5 whisper-base regions of the stress clip
left >= 2 s of loud audio with no token in it, against 1.5 s worst case on the
healthy ones. A pause never registers, however long, because loud is measured
against the region's own speech.

That only says anything about a region that *has* speech, so `has_speech`
guards it: a region of nothing but room tone is loud relative to itself at any
level, and would otherwise spend the whole ladder looking for words nobody
said - the shipped hold-to-talk window before the user starts speaking.

The nudge is a lottery per window, so callers try `RETRY_PADS` in order and
keep whichever decode leaves the least speech untranscribed.
"""

from __future__ import annotations

import numpy as np
from numpy.typing import NDArray

# A stretch of loud audio with no token in it is never legitimate; 2 s is well
# clear of the pauses and timestamp coarseness of a healthy decode.
UNTRANSCRIBED_GAP_S = 2.0
RETRY_PADS = (0.2, 0.3)

_RATE = 16_000
_FRAME = 480  # 30 ms, the VAD's frame
# Loud relative to the region's own speech.
_LOUD_RATIO = 0.25
# Speech is modulated and room tone is not, so a region counts as speech only
# when its loud tenth stands clear of its own quiet tenth. Measured p90/p10:
# 1.09 for noise at any level, 4.9 for the worst clip of the balanced corpus
# and 5.6 for the worst region of the deliberately gapless stress clip. The
# absolute minimum is _AdaptiveVad's own speech threshold (strategies.py).
_SPEECH_OVER_FLOOR = 3.0
_SPEECH_FLOOR = 0.004


def _frame_rms(samples: NDArray[np.float32]) -> NDArray[np.float32] | None:
    frames = len(samples) // _FRAME
    if not frames:
        return None
    block = samples[: frames * _FRAME].reshape(-1, _FRAME)
    return np.sqrt(np.mean(block * block, axis=1))


def _speech_level(rms: NDArray[np.float32]) -> float:
    """The level the region's speech sits at, or 0.0 when it holds none."""
    peak = float(np.percentile(rms, 90))
    floor = float(np.percentile(rms, 10))
    if peak < max(floor * _SPEECH_OVER_FLOOR, _SPEECH_FLOOR):
        return 0.0
    return peak


def has_speech(samples: NDArray[np.float32]) -> bool:
    """Is there anything in ``samples`` a decode could have transcribed?

    Callers use this to decide whether a decode that produced little or
    nothing is worth retrying at all. Room tone is not, however loud."""
    rms = _frame_rms(samples)
    return rms is not None and _speech_level(rms) > 0.0


def untranscribed_gap(samples: NDArray[np.float32], spans: list[tuple[float, float]]) -> float:
    """The longest stretch of loud audio in ``samples`` that no token covers.

    ``spans`` are the (start, end) seconds each token or word accounts for,
    from the start of ``samples`` and in order. A caller passes the times it
    can trust: a token timed only at its onset accounts for (t, t), and so
    does an aligned whisper word, whose *end* stretches across a stretch the
    decode skipped and would hide it. A segment-level time, where one span
    stands for the words inside it, accounts for the whole span.

    A region with no speech in it has no gap, whatever its level."""
    rms = _frame_rms(samples)
    if rms is None:
        return 0.0
    frames = len(rms)
    speech = _speech_level(rms)
    if not speech:
        return 0.0
    loud = speech * _LOUD_RATIO
    total = len(samples) / _RATE
    worst = 0.0
    covered = 0.0
    for start, end in [*spans, (total, total)]:
        if start - covered > worst:
            lo = min(max(int(covered * _RATE) // _FRAME, 0), frames - 1)
            hi = max(lo + 1, min(int(start * _RATE) // _FRAME, frames))
            if float(np.median(rms[lo:hi])) > loud:
                worst = start - covered
        covered = max(covered, end)
    return worst
