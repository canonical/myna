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
# Loud relative to the region's own speech, with an absolute floor so a region
# of pure silence is not measured against itself.
_LOUD_RATIO = 0.25
_LOUD_FLOOR = 0.004


def untranscribed_gap(samples: NDArray[np.float32], spans: list[tuple[float, float]]) -> float:
    """The longest stretch of loud audio in ``samples`` that no token covers.

    ``spans`` are the (start, end) seconds each token or word accounts for,
    from the start of ``samples`` and in order. A caller passes the times it
    can trust: a token timed only at its onset accounts for (t, t), and so
    does an aligned whisper word, whose *end* stretches across a stretch the
    decode skipped and would hide it. A segment-level time, where one span
    stands for the words inside it, accounts for the whole span."""
    frames = len(samples) // _FRAME
    if not frames:
        return 0.0
    block = samples[: frames * _FRAME].reshape(-1, _FRAME)
    rms = np.sqrt(np.mean(block * block, axis=1))
    loud = max(float(np.percentile(rms, 90)) * _LOUD_RATIO, _LOUD_FLOOR)
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
