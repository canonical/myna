"""Rolling-window bounds independent of committed text (audio review A3).

Retention and every decode input must stay within ``window_cap_seconds``
whatever the decoder says: long silence with an empty decoder, a repeated
hypothesis, no stable prefix, words that only arrive late. Forced boundaries
must not lose, duplicate or mis-time a word. The window is observed through
its byte buffer and the decode inputs, so these tests do not depend on the
loop's internal bookkeeping.
"""

from __future__ import annotations

from collections.abc import Callable

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import Disposition, PcmChunk, TranscriptionFinal, TranscriptionProgress
from myna.core.audio import AudioFormat
from myna.testbed.harness import StreamingTelemetry
from myna.testbed.streaming import loop as loop_module
from myna.testbed.streaming.strategies import Hypothesis, LocalAgreement, SilenceCut, Word

RATE = 16_000
FORMAT = AudioFormat(sample_rate_hz=RATE, channels=1, sample_width_bytes=2)


def _ramp(first: int, n: int) -> np.ndarray:
    """PCM whose value encodes its absolute sample index, so a decode input
    can prove it starts where its offset says."""
    return (((np.arange(first, first + n, dtype=np.int64) * 7) % 20_000) - 10_000).astype(np.int16)


async def _audio(seconds: float, chunk_seconds: float, *, ramp: bool = True):
    total = round(seconds * RATE)
    step = round(chunk_seconds * RATE)
    for first in range(0, total, step):
        n = min(step, total - first)
        data = _ramp(first, n) if ramp else np.zeros(n, dtype=np.int16)
        yield PcmChunk(data=data.tobytes(), format=FORMAT)


class _Decoder:
    """Records every decode input and answers from a word timeline.

    A word is recognised when its start lies inside the decoded audio; its end
    is clipped to the end of that audio, like a model that hears only part of
    a word straddling the window edge."""

    def __init__(
        self,
        timeline: Callable[[int], list[Word]] = lambda _call: [],
        *,
        ramp: bool = True,
    ) -> None:
        self._timeline = timeline
        self._ramp = ramp
        self.inputs: list[tuple[int, int]] = []

    def __call__(self, samples: np.ndarray, offset: float) -> Hypothesis:
        first = round(offset * RATE)
        assert abs(offset * RATE - first) < 1e-6, f"offset {offset} is not on a sample"
        if self._ramp and len(samples):
            got = np.round(samples[[0, -1]] * 32768).astype(np.int64).tolist()
            want = _ramp(first, len(samples))[[0, -1]].astype(np.int64).tolist()
            assert got == want, f"decode input at {offset} s is misaligned: {got} != {want}"
        self.inputs.append((first, len(samples)))
        end = offset + len(samples) / RATE
        return Hypothesis(
            words=[
                Word(w.text, w.start, min(w.end, end))
                for w in self._timeline(len(self.inputs))
                if offset <= w.start < end
            ]
        )

    @property
    def largest_input(self) -> int:
        return max(n for _, n in self.inputs)


class _TrackedBuffer(bytearray):
    def __init__(self, high_water: list[int]) -> None:
        super().__init__()
        self._high_water = high_water

    def _note(self) -> None:
        self._high_water[0] = max(self._high_water[0], len(self))

    def extend(self, data) -> None:  # type: ignore[override]
        super().extend(data)
        self._note()

    def __iadd__(self, data):  # type: ignore[override]
        result = super().__iadd__(data)
        self._note()
        return result


@pytest.fixture
def retained(monkeypatch) -> list[int]:
    """High-water mark of the window's raw PCM buffer, in bytes."""
    high_water = [0]
    base = loop_module.RollingWindow

    class _Spy(base):  # type: ignore[misc, valid-type]
        def __init__(self, *args, **kwargs) -> None:
            super().__init__(*args, **kwargs)
            self._buf = _TrackedBuffer(high_water)

    monkeypatch.setattr(loop_module, "RollingWindow", _Spy)
    return high_water


async def _run(
    audio, decoder, strategy, *, cap: float, overlap: float = 1.0, cadence: float = 1.0, **kwargs
):
    events: list[object] = []

    async def emit(event: object) -> None:
        events.append(event)

    transcript = await loop_module.run_streaming_loop(
        audio,
        emit,
        decoder,
        strategy,
        cadence_seconds=cadence,
        window_cap_seconds=cap,
        overlap_seconds=overlap,
        **kwargs,
    )
    return events, transcript


def _committed(events) -> list[str]:
    return [
        e.text
        for e in events
        if isinstance(e, TranscriptionFinal) and e.disposition == Disposition.COMMITTED
    ]


def _spaced(seconds: float, durations=(0.3, 0.45, 0.9, 0.2), gap: float = 0.25) -> list[Word]:
    """Words of varied length back to back, labelled in order, so some of them
    straddle any cut."""
    words, t, k = [], 0.1, 0
    while t < seconds:
        duration = durations[k % len(durations)]
        words.append(Word(f" w{k}", round(t, 4), round(t + duration, 4)))
        t += duration + gap
        k += 1
    return words


def _labels(n: int) -> list[str]:
    return [f"w{k}" for k in range(n)]


def _progress(events) -> int:
    return sum(isinstance(e, TranscriptionProgress) for e in events)


async def _speech_audio(plan, chunk_seconds: float | None = 1.0):
    """(seconds, speech) segments: noise the VAD arms on, or digital silence.
    ``chunk_seconds=None`` appends each segment whole."""
    rng = np.random.default_rng(3)
    parts = []
    for seconds, speech in plan:
        n = round(seconds * RATE)
        if speech:
            noise = rng.standard_normal(n)
            parts.append(
                (noise * (0.05 / np.sqrt(np.mean(noise * noise))) * 32767).astype(np.int16)
            )
        else:
            parts.append(np.zeros(n, np.int16))
    if chunk_seconds is None:
        for part in parts:
            yield PcmChunk(data=part.tobytes(), format=FORMAT)
        return
    pcm = np.concatenate(parts)
    step = round(chunk_seconds * RATE)
    for first in range(0, len(pcm), step):
        yield PcmChunk(data=pcm[first : first + step].tobytes(), format=FORMAT)


# ---------------------------------------------------------------------------
# Retention bounds that text output cannot defeat
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_long_silence_with_an_empty_decoder_stays_within_the_cap(retained):
    decoder = _Decoder(ramp=False)
    _, transcript = await _run(_audio(120.0, 0.1, ramp=False), decoder, LocalAgreement(), cap=5.0)

    assert transcript == ""
    assert decoder.largest_input <= 5 * RATE
    assert retained[0] <= 5 * RATE * 2


@pytest.mark.asyncio
async def test_large_silent_chunks_cannot_push_a_chunked_decode_past_the_cap(retained):
    decoder = _Decoder(ramp=False)
    _, transcript = await _run(_audio(300.0, 25.0, ramp=False), decoder, SilenceCut(), cap=65.0)

    assert transcript == ""
    assert decoder.largest_input <= 65 * RATE
    assert retained[0] <= 65 * RATE * 2


@pytest.mark.asyncio
async def test_a_chunked_strategy_is_bounded_by_the_cap_not_only_its_force_cut(retained):
    decoder = _Decoder(ramp=False)
    await _run(_audio(90.0, 0.1, ramp=False), decoder, SilenceCut(force_cut_seconds=60.0), cap=12.0)

    assert decoder.largest_input <= 12 * RATE
    assert retained[0] <= 12 * RATE * 2


@pytest.mark.asyncio
async def test_a_repeated_hypothesis_cannot_pin_the_window(retained):
    class _Repeating(_Decoder):
        def __call__(self, samples, offset):
            super().__call__(samples, offset)
            return Hypothesis(words=[Word(" hello", offset + 0.2, offset + 0.6)])

    decoder = _Repeating()
    events, transcript = await _run(_audio(60.0, 0.1), decoder, LocalAgreement(), cap=5.0)

    assert transcript == "hello"
    assert _committed(events) == ["hello"]
    assert decoder.largest_input <= 5 * RATE
    assert retained[0] <= 5 * RATE * 2


@pytest.mark.asyncio
async def test_no_stable_prefix_still_commits_every_word_exactly_once(retained):
    """Timestamps jitter by 0.4 s between passes, past AGREE_DRIFT_S, so local
    agreement never commits: only forced boundaries and the tail do."""
    seconds = 40.0

    def jittered(call: int) -> list[Word]:
        shift = 0.2 if call % 2 else -0.2
        return [Word(f" w{p}", p + 0.25 + shift, p + 0.65 + shift) for p in range(int(seconds))]

    decoder = _Decoder(jittered)
    _, transcript = await _run(_audio(seconds, 0.1), decoder, LocalAgreement(), cap=5.0)

    assert transcript.split() == _labels(int(seconds))
    assert decoder.largest_input <= 5 * RATE
    assert retained[0] <= 5 * RATE * 2


@pytest.mark.asyncio
async def test_words_arriving_after_a_long_silence_are_committed_once(retained):
    late = [Word(f" w{k}", 60.2 + k, 60.6 + k) for k in range(3)]
    decoder = _Decoder(lambda _call: late)
    _, transcript = await _run(_audio(64.0, 0.1), decoder, LocalAgreement(), cap=5.0)

    assert transcript.split() == _labels(3)
    assert decoder.largest_input <= 5 * RATE
    assert retained[0] <= 5 * RATE * 2


@pytest.mark.asyncio
@pytest.mark.parametrize("seconds", [20.0, 80.0, 320.0])
async def test_raw_audio_retention_plateaus_as_the_utterance_grows(retained, seconds):
    decoder = _Decoder(ramp=False)
    await _run(_audio(seconds, 1.0, ramp=False), decoder, LocalAgreement(), cap=5.0)

    assert retained[0] == 5 * RATE * 2


# ---------------------------------------------------------------------------
# Forced boundaries: every word once, sample-exact offsets
# ---------------------------------------------------------------------------


def _strategies():
    return [
        pytest.param(LocalAgreement, id="local-agreement"),
        pytest.param(lambda: SilenceCut(force_cut_seconds=600.0), id="silence-cut"),
    ]


@pytest.mark.asyncio
@pytest.mark.parametrize("make_strategy", _strategies())
@pytest.mark.parametrize("chunk_seconds", [0.1, 3.7, 31.0])
async def test_forced_cuts_preserve_every_word_exactly_once(retained, make_strategy, chunk_seconds):
    """No cadence tick fires, so every commit comes from a forced boundary or
    the tail. Words longer than the overlap and words straddling a cut both
    survive, in order, once."""
    seconds = 31.0
    timeline = _spaced(seconds - 0.5)
    decoder = _Decoder(lambda _call: timeline)
    events, transcript = await _run(
        _audio(seconds, chunk_seconds), decoder, make_strategy(), cap=5.0, cadence=1_000.0
    )

    assert transcript.split() == _labels(len(timeline))
    assert "".join(_committed(events)) == transcript
    assert decoder.largest_input <= 5 * RATE
    assert retained[0] <= 5 * RATE * 2
    assert len(decoder.inputs) > 5, "expected several forced boundaries"


@pytest.mark.asyncio
async def test_a_window_filled_exactly_to_the_cap_is_decoded_whole():
    decoder = _Decoder()
    await _run(_audio(5.0, 1.0), decoder, LocalAgreement(), cap=5.0, cadence=1_000.0)

    assert decoder.inputs == [(0, 5 * RATE)]


@pytest.mark.asyncio
async def test_the_cut_waits_for_audio_that_would_overflow_the_window():
    decoder = _Decoder()
    await _run(_audio(10.0, 5.0), decoder, LocalAgreement(), cap=5.0, cadence=1_000.0)

    # Cut at 5 s keeps 1 s; the second chunk fills [4, 9); cut at 9 keeps
    # [8, 9); the last second lands and the tail decodes [8, 10).
    assert decoder.inputs == [(0, 5 * RATE), (4 * RATE, 5 * RATE), (8 * RATE, 2 * RATE)]


@pytest.mark.asyncio
async def test_many_cuts_inside_one_large_append():
    timeline = _spaced(22.5)
    decoder = _Decoder(lambda _call: timeline)
    _, transcript = await _run(
        _audio(23.0, 23.0), decoder, LocalAgreement(), cap=5.0, cadence=1_000.0
    )

    assert decoder.inputs == [
        (0, 5 * RATE),
        (4 * RATE, 5 * RATE),
        (8 * RATE, 5 * RATE),
        (12 * RATE, 5 * RATE),
        (16 * RATE, 5 * RATE),
        (20 * RATE, 3 * RATE),
    ]
    assert transcript.split() == _labels(len(timeline))


@pytest.mark.asyncio
async def test_a_short_tail_after_a_cut_is_decoded_with_its_overlap():
    tail_word = Word(" w0", 5.02, 5.09)
    decoder = _Decoder(lambda _call: [tail_word])
    _, transcript = await _run(
        _audio(5.1, 5.1), decoder, LocalAgreement(), cap=5.0, cadence=1_000.0
    )

    assert decoder.inputs == [(0, 5 * RATE), (4 * RATE, round(1.1 * RATE))]
    assert transcript == "w0"


@pytest.mark.asyncio
async def test_a_short_tail_after_a_cut_is_decoded_even_without_overlap():
    """MIN_DECODE_S skips a whole utterance too short to bother with, never
    the unprocessed remainder a cut left behind."""
    tail_word = Word(" w0", 5.02, 5.09)
    decoder = _Decoder(lambda _call: [tail_word])
    _, transcript = await _run(
        _audio(5.1, 5.1), decoder, LocalAgreement(), cap=5.0, overlap=0.0, cadence=1_000.0
    )

    assert decoder.inputs == [(0, 5 * RATE), (5 * RATE, round(0.1 * RATE))]
    assert transcript == "w0"


@pytest.mark.asyncio
async def test_an_utterance_shorter_than_the_decode_floor_is_still_skipped():
    decoder = _Decoder()
    _, transcript = await _run(_audio(0.2, 0.1), decoder, LocalAgreement(), cap=5.0)

    assert decoder.inputs == []
    assert transcript == ""


@pytest.mark.asyncio
async def test_an_utterance_exactly_at_the_decode_floor_is_decoded():
    decoder = _Decoder()
    await _run(_audio(0.3, 0.1), decoder, LocalAgreement(), cap=5.0)

    assert decoder.inputs == [(0, round(0.3 * RATE))]


@pytest.mark.asyncio
async def test_a_chunked_cut_shorter_than_the_decode_floor_waits():
    """A cut less than MIN_DECODE_S past the window start is not taken; the
    next one that reaches the floor is."""
    decoder = _Decoder()
    await _run(
        _audio(1.0, 0.1),
        decoder,
        SilenceCut(force_cut_seconds=0.2),
        cap=5.0,
        overlap=0.0,
        cadence=1_000.0,
    )

    assert decoder.inputs == [(0, 4800), (4800, 4800), (9600, 4800), (14400, 1600)]


@pytest.mark.asyncio
async def test_a_forced_boundary_holds_back_only_what_the_next_window_redecodes():
    timeline = [Word(" a", 1.0, 1.4), Word(" b", 4.2, 4.8)]
    decoder = _Decoder(lambda _call: timeline)
    events, _ = await _run(_audio(6.0, 1.0), decoder, LocalAgreement(), cap=5.0, cadence=1_000.0)

    assert _committed(events) == ["a", " b"]


@pytest.mark.asyncio
async def test_a_chunked_strategy_still_hears_pauses_after_a_forced_boundary():
    """The pause arrives in the same append that forced the boundary, so the
    VAD must resume scanning exactly at the cut."""
    decoder = _Decoder(ramp=False)
    await _run(
        _speech_audio([(5.0, True), (1.0, False), (2.0, True)], chunk_seconds=None),
        decoder,
        SilenceCut(arm_seconds=0.5, force_cut_seconds=600.0),
        cap=5.0,
    )

    assert len(decoder.inputs) == 3, decoder.inputs
    assert decoder.inputs[0] == (0, 5 * RATE)
    first, n = decoder.inputs[1]
    assert first == 4 * RATE
    assert 5.5 * RATE <= first + n <= 6.0 * RATE


@pytest.mark.asyncio
async def test_a_quiet_re_decode_tick_reports_progress():
    events, _ = await _run(_audio(3.0, 1.0), _Decoder(), LocalAgreement(), cap=5.0)

    assert events == [TranscriptionProgress()] * 3


@pytest.mark.asyncio
async def test_an_empty_hypothesis_does_not_blank_the_display():
    decoder = _Decoder(lambda call: [Word(" a", 0.1, 0.3)] if call == 1 else [])
    events, _ = await _run(_audio(2.0, 1.0), decoder, LocalAgreement(), cap=5.0)

    assert events == [
        TranscriptionFinal(text="a", disposition=Disposition.UNSTABLE),
        TranscriptionProgress(),
    ]


@pytest.mark.asyncio
async def test_unchanged_unstable_text_is_emitted_once():
    class _TrailingWord(_Decoder):
        def __call__(self, samples, offset):
            super().__call__(samples, offset)
            end = offset + len(samples) / RATE
            return Hypothesis(words=[Word(" a", end - 0.3, end - 0.1)])

    events, _ = await _run(_audio(3.0, 1.0), _TrailingWord(), LocalAgreement(), cap=5.0)

    assert events == [
        TranscriptionFinal(text="a", disposition=Disposition.UNSTABLE),
        TranscriptionProgress(),
        TranscriptionProgress(),
        TranscriptionFinal(text="a", disposition=Disposition.COMMITTED, segment_index=0),
    ]


@pytest.mark.asyncio
async def test_chunked_partials_start_at_the_decode_floor_without_a_leading_space():
    telemetry = StreamingTelemetry()
    decoder = _Decoder(lambda _call: [Word(" a", 0.05, 0.15)])
    events, _ = await _run(
        _audio(0.9, 0.3),
        decoder,
        SilenceCut(force_cut_seconds=600.0),
        cap=5.0,
        partial_cadence_seconds=0.3,
        telemetry=telemetry,
    )

    assert [d.window_seconds for d in telemetry.samples if d.kind == "partial"] == [0.3, 0.6, 0.9]
    assert _committed(events) == ["a"]
    assert [e.text for e in events if isinstance(e, TranscriptionFinal)][0] == "a"


@pytest.mark.asyncio
async def test_an_empty_chunked_partial_reports_progress():
    events, _ = await _run(
        _audio(3.0, 1.0),
        _Decoder(),
        SilenceCut(force_cut_seconds=600.0),
        cap=5.0,
        partial_cadence_seconds=1.0,
    )

    assert events == [TranscriptionProgress()] * 3


# ---------------------------------------------------------------------------
# Liveness and telemetry around cuts
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_a_chunk_that_takes_a_forced_boundary_skips_its_tick():
    decoder = _Decoder(ramp=False)
    events, _ = await _run(
        _audio(12.0, 1.0, ramp=False), decoder, SilenceCut(force_cut_seconds=600.0), cap=5.0
    )

    # Boundaries land on the 6th and 10th chunks; every other chunk ticks.
    assert len(decoder.inputs) == 3
    assert _progress(events) == 10


@pytest.mark.asyncio
async def test_a_chunked_strategy_ticks_on_its_cadence_not_on_every_chunk():
    decoder = _Decoder(ramp=False)
    events, _ = await _run(
        _audio(12.0, 0.5, ramp=False), decoder, SilenceCut(force_cut_seconds=600.0), cap=5.0
    )

    assert _progress(events) == 12


@pytest.mark.asyncio
async def test_a_chunk_that_takes_a_pause_cut_skips_its_tick():
    decoder = _Decoder(ramp=False)
    events, _ = await _run(
        _speech_audio([(16.0, True), (1.0, False), (4.0, True)]), decoder, SilenceCut(), cap=65.0
    )

    assert len(decoder.inputs) == 2
    assert _progress(events) == 20


@pytest.mark.asyncio
async def test_a_tick_that_commits_is_not_reported_as_quiet():
    decoder = _Decoder(lambda _call: [Word(" a", 0.0, 0.4)])
    events, _ = await _run(_audio(2.0, 1.0), decoder, LocalAgreement(), cap=5.0)

    assert [(type(e).__name__, getattr(e, "text", None)) for e in events] == [
        ("TranscriptionFinal", "a"),
        ("TranscriptionFinal", "a"),
    ]
    assert [e.disposition for e in events] == [Disposition.UNSTABLE, Disposition.COMMITTED]


@pytest.mark.asyncio
async def test_telemetry_records_every_decode_including_forced_boundaries():
    telemetry = StreamingTelemetry()
    await _run(_audio(6.0, 1.0), _Decoder(), LocalAgreement(), cap=5.0, telemetry=telemetry)

    assert [(d.kind, d.window_seconds) for d in telemetry.samples] == [
        ("tick", 1.0),
        ("tick", 2.0),
        ("tick", 3.0),
        ("tick", 4.0),
        ("tick", 5.0),
        ("commit", 5.0),
        ("tick", 2.0),
        ("commit", 2.0),
    ]
    assert all(0.0 <= d.wall_seconds < 60.0 for d in telemetry.samples)
    assert telemetry.audio_seconds_ingested == 6.0
    assert 0.0 <= telemetry.session_seconds < 60.0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("tail", "partials"),
    [(None, [1.0, 2.0, 3.0, 4.0, 5.0, 3.0]), (2.0, [1.0, 2.0, 2.0, 2.0, 2.0, 2.0])],
)
async def test_telemetry_records_chunked_partials(tail, partials):
    telemetry = StreamingTelemetry()
    await _run(
        _audio(7.0, 1.0),
        _Decoder(),
        SilenceCut(force_cut_seconds=600.0),
        cap=5.0,
        partial_cadence_seconds=1.0,
        partial_tail_seconds=tail,
        telemetry=telemetry,
    )

    assert [d.window_seconds for d in telemetry.samples if d.kind == "partial"] == partials
    assert [d.window_seconds for d in telemetry.samples if d.kind == "commit"] == [5.0, 3.0]


@pytest.mark.asyncio
async def test_nothing_is_decoded_again_once_the_last_cut_covered_all_audio():
    decoder = _Decoder(ramp=False)
    await _run(_audio(60.0, 1.0, ramp=False), decoder, SilenceCut(), cap=65.0)

    assert decoder.inputs == [(0, 60 * RATE)]


@pytest.mark.asyncio
async def test_a_chunked_strategy_takes_every_pause_inside_one_append():
    rng = np.random.default_rng(3)

    def speech(seconds: float) -> np.ndarray:
        n = round(seconds * RATE)
        noise = rng.standard_normal(n)
        return (noise * (0.05 / np.sqrt(np.mean(noise * noise))) * 32767).astype(np.int16)

    pcm = np.concatenate(
        [
            speech(16.0),
            np.zeros(RATE, np.int16),
            speech(16.0),
            np.zeros(RATE, np.int16),
            speech(2.0),
        ]
    )

    async def one_append():
        yield PcmChunk(data=pcm.tobytes(), format=FORMAT)

    decoder = _Decoder(ramp=False)
    await _run(one_append(), decoder, SilenceCut(), cap=65.0)

    assert len(decoder.inputs) == 3, f"expected two pause cuts and a tail: {decoder.inputs}"
    assert decoder.largest_input <= 19 * RATE
