"""The streaming emission loop (feature 008).

Drives a RollingWindow + commit strategy over a live PCM chunk stream.
Re-decode strategies (local-agreement): the uncommitted window is re-decoded
on a cadence and the strategy decides what to commit when. Chunked policies
(SilenceCut, murmure-style — Parakeet TDT): the loop watches the audio for a
silence/force cut; only then is the region up to the cut decoded *once* and
committed wholesale — no re-decode, so committed text costs one decode per
chunk. Between cuts, ``partial_cadence_seconds`` re-decodes the tail of the
uncommitted window for *display* (see [`_chunked_partial`]): committed text is
untouched by it, and without it a chunked strategy shows nothing until its
first cut, which at the shipped 15 s arm is most of a minute in the worst
case. Emits 007-contract events (committed finals with
monotonic ``segment_index``; unstable finals that supersede the previous
unstable) and returns the accumulated committed transcript — the caller
emits ``TranscriptionDone``.

Invariants enforced here (contracts/emission-semantics.md):
- I1/I2: committed text is append-only; the returned transcript is exactly
  the **verbatim** concatenation of committed emissions — chunks keep their
  natural inter-word whitespace (whisper word/segment texts carry leading
  spaces); only the utterance's first chunk sheds its leading space. A
  consumer reconstructs the transcript by concatenating, never by joining.
- I3: unstable emissions never carry a segment_index and never touch
  committed text. Unstable display text is the *uncommitted remainder* of the
  current hypothesis (overlap-deduped like commits) — it never restates text
  a previous commit already emitted.
- I4/I5: end-of-audio resolves the outstanding unstable tail — the remainder
  is committed (or dropped if empty) before return.
- I6: RollingWindow bounds the uncommitted buffer (over-cap forces commits).

The ``decode`` callable is injectable so the loop is testable without a model
and reusable across adapters (whisper re-decode, parakeet chunk-commit).
Signature: ``decode(samples: np.ndarray, offset_seconds: float) -> Hypothesis``
with word/segment times in *absolute* session seconds (offset added). It runs
in a worker thread.

``telemetry`` (perf T03, ``myna.testbed.harness.StreamingTelemetry``) is an
optional accumulator that records every decode call's kind/window/wall time
without touching commit or alignment logic - ``None`` on every production
call path, so it costs nothing there.
"""

from __future__ import annotations

import asyncio
import functools
import time
from collections.abc import AsyncIterator, Awaitable, Callable
from concurrent.futures import ThreadPoolExecutor
from contextlib import nullcontext

import numpy as np

from myna.core import (
    Disposition,
    EventSink,
    PcmChunk,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.harness import StreamingTelemetry

from .strategies import Hypothesis, Word
from .window import RATE, RollingWindow

# Tracy frame marks (dev tooling only, see myna.testbed.parakeet._TRACY):
# one frame per decode call, named by kind, so Tracy's frame-time view shows
# the streaming duty cycle directly - gaps between frames are idle time,
# frame width is decode wall time.
try:
    from tracy_client import ScopedFrame as _TracyFrame

    _TRACY = True
except ImportError:
    _TRACY = False


def _frame(name: str):
    return _TracyFrame(name) if _TRACY else nullcontext()


MIN_DECODE_S = 0.3  # don't bother decoding sub-300ms tails
_OVERLAP_LOOKBACK = 12  # words of committed history text-alignment dedupe uses


# Minimum character overlap for the alignment to act: 1-char matches are too
# weak to drop on (single letters are everywhere); shorter matches fall back
# to the timestamp signal.
_MIN_OVERLAP_CHARS = 2

# Old content in a re-decode is physically bounded: it re-transcribes only
# the RollingWindow's 1 s of pre-frontier overlap audio, so the duplicate
# region is always a short *prefix* of the new hypothesis. At conversational
# speech rates the overlap holds ~2-3 words; 5 is a generous bound (300 wpm
# for a full second). A claimed duplicate region LONGER than that is new
# text coincidentally repeating committed text, and the alignment must
# abstain — matching there eats genuinely-new words (observed 2026-07-28,
# stream-2277-02/tail-mutation: committed "...the ball. No answer." +
# decode "he rang again this time harder still no answer" — the clipped
# overlap never re-surfaced the OLD "no answer", so the only match was the
# genuinely-new final one and the whole 9-word tail dropped). If
# `overlap_seconds` ever grows past ~2 s this bound should grow with it.
_MAX_OVERLAP_WORDS = 5


def _squash(text: str) -> str:
    """Lowercased alphanumerics only — word boundaries and punctuation vanish,
    so merged/split re-decode tokens still align with the words they came
    from ("es"+"Carlos." ≈ "escarlos")."""
    return "".join(c for c in text.lower() if c.isalnum())


def _alignment_drop(tail: list[str], new: list[str]) -> int:
    """How many leading words of `new` correspond to already-committed text.

    Anchored on the *committed frontier*, at **character** level: word texts
    are squashed ([`_squash`]) and the longest suffix of the committed tail
    occurring in the new stream (leftmost occurrence) marks the duplicate
    region — everything up to its end is old. Character matching survives
    the two ways word-level matching fails:
    - decode churn inside the overlap window ("in some way or" → "and some
      white or" — the exact prefix==suffix match returned 0: "or or");
    - word-boundary churn after a pause — the re-decode merges committed
      words into one token ("es"+"Carlos." → " escarlos.") or splits them
      (observed live 2026-07-28: "escarlos." re-committed).
    Only *fully covered* words drop — a partial cover means the match ended
    mid-word (e.g. inside a genuinely new word), which stays. Greedy global
    matchers (difflib) are avoided deliberately: they can partition away the
    frontier run when genuinely-new words after it match older committed
    words.

    The claimed duplicate region is bounded at [`_MAX_OVERLAP_WORDS`]
    words: old content re-transcribes only the window's 1 s overlap audio,
    so it is always a short prefix of the hypothesis. A match implying a
    longer drop is new text coincidentally repeating committed text — the
    alignment ABSTAINS (returns 0) rather than dropping new words, and does
    not fall through to shorter suffixes: those would match the same
    out-of-region text more loosely (a 2-char suffix like "er" lives inside
    "harder"), compounding the loss. Under-dropping leaks a visible
    duplicate; over-dropping silently loses words — on ambiguity, keep the
    words.
    """
    if not tail or not new:
        return 0
    tail_squash = _squash("".join(tail))
    parts = [_squash(w) for w in new]
    new_squash = "".join(parts)
    if not tail_squash or not new_squash:
        return 0
    end = 0
    max_k = min(len(tail_squash), len(new_squash))
    for k in range(max_k, _MIN_OVERLAP_CHARS - 1, -1):
        j = new_squash.find(tail_squash[-k:])
        if j < 0:
            continue  # this length absent; try a shorter suffix
        end = j + k
        break
    if not end:
        return 0
    drop = 0
    covered = 0
    for part in parts:
        if part and covered + len(part) <= end:
            covered += len(part)
            drop += 1
        else:
            break
    if drop > _MAX_OVERLAP_WORDS:
        # The match claims more words than the overlap audio can hold — it
        # is new text repeating committed text (see _MAX_OVERLAP_WORDS).
        # Abstain entirely: no shorter-suffix retry (same spurious region,
        # looser match), no partial drop (the match is either wholly the
        # overlap re-transcription or wholly coincidence).
        return 0
    return drop


def _drop_committed(
    words: list,
    committed_word_texts: list[str],
    committed_through: float,
) -> list:
    """Overlap dedupe (I2): drop words a previous commit already emitted.

    Two signals, unioned (a word is dropped if EITHER marks it old):
    (a) timestamp — the word ends at/before the committed coverage;
    (b) character-level text alignment — the word sits inside the duplicate
    region reaching the committed frontier ([`_alignment_drop`]). The
    alignment drop is **not** gated on timestamps: after a long silence the
    (VAD-free) re-decode compresses the pause and re-times already-committed
    overlap words arbitrarily late — observed live 2026-07-28 ("quite well."
    + [16 s silence] + "My name is Charlie" injected as "quite well. well.
    My name is Charlie") — so alignment is the only trustworthy dedupe
    signal there. The accepted trade-off: a genuinely *new* word that exactly
    repeats the frontier suffix is dropped when the overlap decode failed to
    re-transcribe the old instance (rare; a visible duplicate on every pause
    is worse, and the leftmost-longest rule keeps later repeats: "no. No
    way." survives).
    """
    tail = committed_word_texts[-_OVERLAP_LOOKBACK:]
    new = [_norm(w.text) for w in words]
    drop = _alignment_drop(tail, new)
    return [w for i, w in enumerate(words) if not (w.end <= committed_through + 1e-3 or i < drop)]


def _norm(text: str) -> str:
    return text.strip().lower().strip(".,!?;:\"'“”")


def _join_natural(words: list) -> str:
    """Verbatim word-text join (natural spacing): whisper word texts carry
    their own leading whitespace, so joining verbatim keeps inter-word
    spaces. Only trailing whitespace is trimmed."""
    return "".join(w.text for w in words).rstrip()


def _utterance_edge(text: str, first: bool) -> str:
    """Strip the leading space of an utterance's *first* emission only —
    later chunks keep theirs so verbatim concatenation reconstructs the
    transcript (I2). Applies to committed and unstable emissions alike, so
    in-field preedit renders correctly after committed text."""
    return text.lstrip() if first else text


async def _chunked_partial(
    window: RollingWindow,
    run_decode: Callable[[np.ndarray, float], Awaitable[Hypothesis]],
    fresh_words: Callable[[list[Word]], list[Word]],
    tail_seconds: float | None,
) -> list[Word] | None:
    """One unstable tick for a chunked strategy — display text only.

    Decodes the whole uncommitted window, so what is shown is always a single
    self-consistent hypothesis that grows as the speaker talks. ``None`` says
    "nothing to show this tick": leave whatever is on screen alone rather than
    blanking it, because an empty decode is more often the encoder faltering
    on this particular window than the speaker having said nothing.

    ``tail_seconds`` caps the decode at the last N seconds for machines that
    need the compute back. It is off by default, and it is a real trade, not a
    free optimisation: the display then shows only those N seconds and drops
    its head as the window slides. Stitching a longer display out of
    successive tails was tried and abandoned (2026-08-28) — each tick can only
    contribute its own first second to the stitched head, and a decode's first
    second is its least reliable part, so the head accumulated into nonsense
    ("Many little wrinkles his brow He could Arranged that Satisfactorily")
    while the full-window decode of the same audio read cleanly.
    """
    if tail_seconds and window.window_seconds > tail_seconds:
        offset = window.end - tail_seconds
        samples = window.samples()[-int(tail_seconds * RATE) :]
    else:
        offset, samples = window.frontier, window.samples()
    hyp = await run_decode(samples, offset)
    return fresh_words(hyp.words) or None


async def run_streaming_loop(
    audio: AsyncIterator[PcmChunk],
    emit: EventSink,
    decode: Callable[[np.ndarray, float], Hypothesis],
    strategy,
    cadence_seconds: float = 1.0,
    window_cap_seconds: float = 30.0,
    overlap_seconds: float = 1.0,
    partial_cadence_seconds: float | None = None,
    partial_tail_seconds: float | None = None,
    telemetry: StreamingTelemetry | None = None,
) -> str:
    # Every decode in a session runs on this one thread. `asyncio.to_thread`
    # would use the event loop's default pool, which grows to min(32, cpu + 4)
    # workers even under a strictly sequential caller — a submit that lands
    # before the previous worker has released the idle semaphore spawns
    # another. Harmless at one decode per utterance; at two per second it
    # reached the cap, and since each thread takes a glibc malloc arena, RSS
    # climbed ~500 MB over a five-minute session. There is nothing to gain
    # from a wider pool: decodes here are strictly sequential and ORT already
    # parallelises inside one.
    executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="myna-decode")

    async def run_decode(samples: np.ndarray, offset: float) -> Hypothesis:
        return await asyncio.get_running_loop().run_in_executor(executor, decode, samples, offset)

    try:
        return await _run(
            audio,
            emit,
            run_decode,
            strategy,
            cadence_seconds,
            window_cap_seconds,
            overlap_seconds,
            partial_cadence_seconds,
            partial_tail_seconds,
            telemetry=telemetry,
        )
    finally:
        executor.shutdown(wait=False)


async def _run(
    audio: AsyncIterator[PcmChunk],
    emit: EventSink,
    run_decode: Callable[[np.ndarray, float], Awaitable[Hypothesis]],
    strategy,
    cadence_seconds: float,
    window_cap_seconds: float,
    overlap_seconds: float,
    partial_cadence_seconds: float | None,
    partial_tail_seconds: float | None,
    telemetry: StreamingTelemetry | None = None,
) -> str:
    # perf T03: additive, outside the commit/alignment logic below -- with
    # telemetry=None (every production call today) this is one branch per
    # decode and costs nothing.
    session_t0 = time.perf_counter() if telemetry is not None else 0.0

    async def timed_decode(samples: np.ndarray, offset: float, kind: str) -> Hypothesis:
        with _frame(f"decode:{kind}"):
            if telemetry is None:
                return await run_decode(samples, offset)
            t0 = time.perf_counter()
            hyp = await run_decode(samples, offset)
            telemetry.record(kind, len(samples) / RATE, time.perf_counter() - t0)
            return hyp

    window = RollingWindow(window_cap_seconds, overlap_seconds)
    committed: list[str] = []
    committed_through = 0.0  # absolute seconds covered by committed text
    committed_word_texts: list[str] = []  # normalized, for text-alignment dedupe
    segment_index = 0
    last_hyp: Hypothesis | None = None
    last_unstable = ""
    last_decode_end = 0.0

    async def emit_committed(text: str, words: list[Word]) -> None:
        nonlocal segment_index
        await emit(
            TranscriptionFinal(
                text=text,
                disposition=Disposition.COMMITTED,
                segment_index=segment_index,
            )
        )
        committed.append(text)
        committed_word_texts.extend(_norm(w.text) for w in words)
        segment_index += 1

    async def emit_unstable(text: str) -> None:
        nonlocal last_unstable
        if text and text != last_unstable:
            await emit(TranscriptionFinal(text=text, disposition=Disposition.UNSTABLE))
            last_unstable = text

    def fresh_words(words: list[Word]) -> list[Word]:
        """Overlap dedupe (I2) — see module-level [`_drop_committed`]."""
        return _drop_committed(words, committed_word_texts, committed_through)

    async for chunk in audio:
        window.append(chunk.data, chunk.duration_seconds)

        if getattr(strategy, "mode", "redecode") == "chunked":
            cut = strategy.observe(window.samples(), window.frontier, window.end)
            if cut is not None and cut - window.frontier >= MIN_DECODE_S:
                samples = window.region_before(cut)
                hyp = await timed_decode(samples, window.frontier, "commit")
                fresh = fresh_words(hyp.words)
                text = _join_natural(fresh)
                if text:
                    await emit_committed(_utterance_edge(text, not committed), fresh)
                # The audio up to the cut is covered (committed, possibly
                # empty for a silence chunk) — the frontier and the dedupe
                # watermark both advance.
                committed_through = max(committed_through, cut)
                window.advance(cut)
                last_unstable = ""  # I4: the commit resolves the epoch
            elif window.end - last_decode_end >= (partial_cadence_seconds or cadence_seconds):
                last_decode_end = window.end
                if partial_cadence_seconds and window.window_seconds >= MIN_DECODE_S:
                    words = await _chunked_partial(
                        window,
                        functools.partial(timed_decode, kind="partial"),
                        fresh_words,
                        partial_tail_seconds,
                    )
                    text = _utterance_edge(_join_natural(words), not committed) if words else ""
                    if text:
                        await emit_unstable(text)
                    else:
                        await emit(TranscriptionProgress())
                else:
                    await emit(TranscriptionProgress())  # liveness on quiet ticks
            continue

        # tick on cadence
        if window.end - last_decode_end < cadence_seconds:
            continue
        last_decode_end = window.end
        hyp = await timed_decode(window.samples(), window.frontier, "tick")
        decision = strategy.commit_rule(last_hyp, hyp, window.end, window.over_cap)
        last_hyp = hyp
        produced = False
        if decision is not None and decision.commit_end > committed_through:
            fresh = fresh_words(list(decision.commit_words))
            text = _join_natural(fresh) if fresh else ""
            if text:
                await emit_committed(_utterance_edge(text, not committed), fresh)
                committed_through = decision.commit_end
                window.advance(decision.commit_end)
                last_unstable = ""  # I4: commit clears unstable
                produced = True
        before = last_unstable
        # Unstable display text = the *uncommitted remainder* of the current
        # hypothesis (same overlap dedupe as commits) — never restates text a
        # previous commit already emitted, and keeps its natural leading space
        # so in-field preedit renders correctly after committed text.
        unstable_tail = _utterance_edge(_join_natural(fresh_words(hyp.words)), not committed)
        await emit_unstable(unstable_tail)
        produced = produced or last_unstable != before
        if not produced:
            await emit(TranscriptionProgress())  # liveness on quiet ticks

    # I5: resolve the tail — decode whatever is left and commit it.
    if window.window_seconds >= MIN_DECODE_S:
        hyp = await timed_decode(window.samples(), window.frontier, "commit")
        fresh = fresh_words(hyp.words)
        tail = _join_natural(fresh)
        if tail:
            await emit_committed(_utterance_edge(tail, not committed), fresh)
    if telemetry is not None:
        telemetry.audio_seconds_ingested = window.end
        telemetry.session_seconds = time.perf_counter() - session_t0
    return "".join(committed)
