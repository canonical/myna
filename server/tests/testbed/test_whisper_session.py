"""Whisper batch-session units - model-free.

The real adapter is covered end-to-end by tests/test_whisper_adapter.py, which
needs the ``whisper`` extra and the tiny weights and so skips on a stock
checkout. That left the emission contract itself untested: what run_session
puts on the wire for a given decode. Following the Parakeet units' house
pattern, a stub stands in for ``WhisperModel`` so the segment -> final
mapping, the I2 verbatim concatenation, the readiness ordering and the
failure path are pinned without loading weights.
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import (
    PHASE_READY,
    AudioFormat,
    Disposition,
    PcmChunk,
    SessionConfig,
    TranscriptionDone,
    TranscriptionError,
    TranscriptionFinal,
    TranscriptionProgress,
)
from myna.testbed.whisper import FasterWhisperAdapter

FORMAT = AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=2)


class _Word:
    """One faster-whisper word (the attributes the adapter reads)."""

    def __init__(self, word, start, end, probability=0.9):
        self.word = word
        self.start = start
        self.end = end
        self.probability = probability


class _Segment:
    """One faster-whisper segment (the attributes the adapter reads).
    ``words`` is None unless the decode asked for the alignment pass."""

    def __init__(self, text, start=0.0, end=1.0, avg_logprob=-0.1, words=None, tokens=()):
        self.text = text
        self.start = start
        self.end = end
        self.avg_logprob = avg_logprob
        self.words = words
        self.tokens = list(tokens)


class _FakeWhisperModel:
    """Stands in for ``faster_whisper.WhisperModel``: yields scripted segments
    instead of decoding. ``transcribe`` returns a generator, as the real one
    does, so the adapter's drain-in-the-worker-thread step is exercised."""

    def __init__(self, *segments):
        self._segments = segments
        self.calls: list[dict] = []

    def transcribe(self, samples, **kwargs):
        self.calls.append({"samples": len(samples), **kwargs})
        return (seg for seg in self._segments), SimpleNamespace(language="en")


def adapter_with(*segments) -> FasterWhisperAdapter:
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _FakeWhisperModel(*segments)  # skips the lazy load
    return adapter


async def pcm_audio(seconds: float, chunk_s: float = 0.5):
    for _ in range(int(seconds / chunk_s)):
        yield PcmChunk(data=b"\x01\x00" * int(16_000 * chunk_s), format=FORMAT)


async def run_session(adapter, audio_seconds: float = 1.0, fmt=FORMAT, **config):
    events = []

    async def emit(event):
        events.append(event)

    cfg = SessionConfig(audio_format=fmt, language="en-GB", **config)
    await adapter.run_session(cfg, pcm_audio(audio_seconds), emit)
    return events


def finals(events):
    return [e for e in events if isinstance(e, TranscriptionFinal)]


async def test_each_segment_commits_and_done_is_their_verbatim_concatenation():
    adapter = adapter_with(_Segment(" hello there"), _Segment(" world"))

    events = await run_session(adapter)

    # I2: only the first final sheds its leading space, so concatenating the
    # deltas verbatim reproduces the transcript - no separator is inserted.
    assert [f.text for f in finals(events)] == ["hello there", " world"]
    assert all(f.disposition is Disposition.COMMITTED for f in finals(events))
    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == "hello there world"


async def test_blank_segments_are_dropped():
    adapter = adapter_with(_Segment("   "), _Segment(" real speech "), _Segment(""))

    events = await run_session(adapter)

    assert [f.text for f in finals(events)] == ["real speech"]
    assert events[-1].text == "real speech"


async def test_ready_is_signalled_before_any_audio_is_pulled():
    """The client gates on ready; pulling audio first deadlocks the session
    before consuming audio."""
    adapter = adapter_with(_Segment(" hi"))
    events = []
    phases_at_first_pull = []

    async def emit(event):
        events.append(event)

    async def audio():
        phases_at_first_pull.extend(e.phase for e in events if isinstance(e, TranscriptionProgress))
        yield PcmChunk(data=b"\x01\x00" * 16_000, format=FORMAT)

    await adapter.run_session(SessionConfig(audio_format=FORMAT), audio(), emit)

    assert PHASE_READY in phases_at_first_pull


async def test_silence_is_finalised_without_troubling_the_model():
    adapter = adapter_with(_Segment(" never decoded"))

    events = await run_session(adapter, audio_seconds=0.0)

    assert isinstance(events[-1], TranscriptionDone)
    assert events[-1].text == ""
    assert adapter._model.calls == []


async def test_progress_ticks_while_audio_is_buffering():
    adapter = adapter_with(_Segment(" hi"))

    events = await run_session(adapter, audio_seconds=3.0)

    # readiness plus a heartbeat per second of buffered audio (interval 1.0 s)
    buffering = [
        e for e in events if isinstance(e, TranscriptionProgress) and e.phase != PHASE_READY
    ]
    assert len(buffering) >= 2


async def test_timestamps_are_attached_only_when_asked_for():
    adapter = adapter_with(_Segment(" hi", start=0.25, end=1.5, avg_logprob=-0.3))

    without = await run_session(adapter)
    assert finals(without)[0].segments == ()

    adapter = adapter_with(_Segment(" hi", start=0.25, end=1.5, avg_logprob=-0.3))
    with_stamps = await run_session(adapter, timestamp_granularity="segment")
    segment = finals(with_stamps)[0].segments[0]
    assert (segment.start, segment.end, segment.score) == (0.25, 1.5, -0.3)
    assert segment.text == "hi"


async def test_asking_for_timestamps_turns_on_the_alignment_pass():
    """Whisper's own segment boundaries are quantised to whole seconds, so a
    timestamp request has to buy the word alignment to be worth anything."""
    adapter = adapter_with(_Segment(" hi"))
    await run_session(adapter)
    assert "word_timestamps" not in adapter._model.calls[0]

    adapter = adapter_with(_Segment(" hi"))
    await run_session(adapter, timestamp_granularity="segment")
    assert adapter._model.calls[0]["word_timestamps"] is True


async def test_segment_timestamps_follow_the_word_alignment():
    """The cue spans the words, not the segment's rounded-off boundaries."""
    adapter = adapter_with(
        _Segment(
            " hi there",
            start=0.0,
            end=2.0,
            words=[_Word(" hi", 0.42, 0.68), _Word(" there", 0.71, 1.13)],
        )
    )

    events = await run_session(adapter, timestamp_granularity="segment")

    (segment,) = finals(events)[0].segments
    assert (segment.start, segment.end) == (0.42, 1.13)
    assert segment.text == "hi there"


async def test_word_granularity_yields_one_entry_per_word():
    adapter = adapter_with(
        _Segment(
            " hi there",
            words=[_Word(" hi", 0.42, 0.68, probability=0.8), _Word(" there", 0.71, 1.13)],
        )
    )

    events = await run_session(adapter, timestamp_granularity="word")

    segments = finals(events)[0].segments
    assert [(s.text, s.start, s.end) for s in segments] == [
        ("hi", 0.42, 0.68),
        ("there", 0.71, 1.13),
    ]
    assert segments[0].score == 0.8


async def test_word_granularity_degrades_to_the_segment_span():
    """A client that asked for timestamps gets one even if alignment produced
    no words."""
    adapter = adapter_with(_Segment(" hi", start=0.25, end=1.5))

    events = await run_session(adapter, timestamp_granularity="word")

    (segment,) = finals(events)[0].segments
    assert (segment.start, segment.end, segment.text) == (0.25, 1.5, "hi")


async def test_region_subtags_are_dropped_for_the_decoder():
    """faster-whisper rejects "en-GB"; the adapter passes bare ISO 639-1."""
    adapter = adapter_with(_Segment(" hi"))

    await run_session(adapter)

    assert adapter._model.calls[0]["language"] == "en"


async def test_a_failed_decode_is_reported_as_an_error_not_a_crash():
    class _Boom:
        def transcribe(self, samples, **kwargs):
            raise RuntimeError("ct2 exploded")

    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _Boom()

    events = await run_session(adapter)

    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "RuntimeError: ct2 exploded" in events[-1].message


def test_a_local_model_directory_is_labelled_by_its_leaf():
    """Snap model components arrive as absolute paths; result records would be
    unreadable if the candidate carried the whole path."""
    adapter = FasterWhisperAdapter("/snap/myna/current/models/faster-whisper-small/")

    assert adapter.candidate.model == "whisper-faster-whisper-small"


def test_english_only_checkpoints_advertise_english_only():
    assert FasterWhisperAdapter("tiny.en").capabilities().languages == ("en",)
    assert FasterWhisperAdapter("tiny").capabilities().languages == ("*",)


def test_streaming_switches_the_advertised_strategy():
    assert FasterWhisperAdapter("tiny").candidate.streaming_strategy == "commit-on-finalize"
    streaming = FasterWhisperAdapter("tiny", streaming=True)
    assert streaming.streaming and streaming.candidate.streaming_strategy == "local-agreement"


async def test_unload_releases_the_model_and_is_idempotent():
    adapter = adapter_with(_Segment(" hi"))

    await adapter.unload()
    assert adapter._model is None
    await adapter.unload()  # idle-unload may fire again before a new session
    assert adapter._model is None


async def test_streaming_re_decodes_the_window_with_word_timestamps():
    """The local-agreement strategy compares successive hypotheses word by
    word, so the streaming decode must ask for word timestamps and hand back
    the whole window each tick. (The strategy itself is covered by
    test_emission_invariants; this pins whisper's wiring into it.)"""

    class _Word:
        def __init__(self, word, start, end):
            self.word, self.start, self.end = word, start, end

    class _WordSegment(_Segment):
        def __init__(self, text, words):
            super().__init__(text)
            self.words = words

    class _StreamingModel:
        def __init__(self):
            self.window_seconds: list[float] = []

        def transcribe(self, samples, **kwargs):
            assert kwargs["word_timestamps"] is True
            self.window_seconds.append(len(samples) / 16_000)
            return (s for s in [_WordSegment(" hi", [_Word(" hi", 0.0, 0.4)])]), None

    adapter = FasterWhisperAdapter("tiny", streaming=True, stream_cadence_s=0.5)
    adapter._model = _StreamingModel()

    events = await run_session(adapter, audio_seconds=2.0)

    assert isinstance(events[-1], TranscriptionDone)
    assert adapter._model.window_seconds  # the decoder was actually driven
    assert adapter._model.window_seconds == sorted(adapter._model.window_seconds)


async def test_a_format_mismatch_is_refused_before_the_model_is_touched():
    adapter = adapter_with(_Segment(" hi"))

    events = await run_session(adapter, fmt=AudioFormat(sample_rate_hz=44_100))

    assert isinstance(events[0], TranscriptionError)
    assert events[0].code == "unsupported_audio_format"
    assert adapter._model.calls == []


# ---------------------------------------------------------------------------
# Batch over long input: bounded regions, deferred presentation (audio review A5)
# ---------------------------------------------------------------------------

RATE = 16_000
_TENTH = RATE // 10


def _speech_pcm(plan) -> np.ndarray:
    """(seconds, speech) spans. Speech samples encode their own position:
    magnitude 8000 + index // 1600 with alternating sign, loud enough that the
    VAD never hears it as a pause. Pauses are digital silence. Span edges must
    fall on tenths of a second."""
    parts, first = [], 0
    for seconds, speech in plan:
        n = round(seconds * RATE)
        assert n % _TENTH == 0
        if speech:
            idx = np.arange(first, first + n)
            sign = np.where(idx % 2, -1, 1)
            parts.append((sign * (8000 + idx // _TENTH)).astype(np.int16))
        else:
            parts.append(np.zeros(n, np.int16))
        first += n
    return np.concatenate(parts)


async def _chunks(pcm: np.ndarray, chunk_seconds: float, done: list[bool]):
    step = round(chunk_seconds * RATE)
    for first in range(0, len(pcm), step):
        yield PcmChunk(data=pcm[first : first + step].tobytes(), format=FORMAT)
    done.append(True)


class _WordTokenizer:
    """Tokenises " wT" words as the token T, as the positional model decodes them."""

    def encode(self, text, add_special_tokens=True):
        assert add_special_tokens is False
        return SimpleNamespace(ids=[int("".join(filter(str.isdigit, w))) for w in text.split()])


class _PositionalModel:
    """A whisper stand-in that hears where its input sits in the utterance.

    It recovers the absolute start of every decode input from the PCM, then
    recognises one word " wT" at [T + 0.1, T + 0.6) for every whole second T
    whose onset lies in the input and in speech, four words to a segment.
    Times are input-relative, like faster-whisper's; a word straddling the end
    of the input is clipped to it."""

    def __init__(self, pcm: np.ndarray, *, language="en"):
        self._pcm = pcm
        self._language = language
        self.calls: list[dict] = []
        self.hf_tokenizer = _WordTokenizer()

    def _locate(self, samples) -> int | None:
        values = np.round(samples * 32768).astype(np.int64)
        nonzero = np.flatnonzero(values)
        if not len(nonzero):
            return None
        k = int(nonzero[0])
        tenth = abs(int(values[k])) - 8000
        run = int(np.argmax(np.abs(values[k:]) != abs(int(values[k])))) or len(values) - k
        return (tenth + 1) * _TENTH - run - k

    onset = 0.1
    duration = 0.5

    def _word(self, t: int, start_s: float, end_s: float) -> _Word | None:
        onset = t + self.onset
        return _Word(f" w{t}", onset - start_s, min(onset + self.duration, end_s) - start_s)

    def transcribe(self, samples, **kwargs):
        first = self._locate(samples)
        self.calls.append({"first": first, "samples": len(samples), **kwargs})
        if first is None:
            return iter(()), SimpleNamespace(language=self._language)
        start_s, end_s = first / RATE, (first + len(samples)) / RATE
        words = []
        for t in range(int(start_s) - 1, int(end_s) + 1):
            onset = t + self.onset
            if start_s <= onset < end_s and self._pcm[round(onset * RATE)] != 0:
                word = self._word(t, start_s, end_s)
                if word is not None:
                    words.append((t, word))
        segments = [
            _Segment(
                "".join(w.word for _, w in group),
                start=group[0][1].start,
                end=group[-1][1].end,
                words=[w for _, w in group] if kwargs.get("word_timestamps") else None,
                tokens=[t for t, _ in group],
            )
            for group in (words[i : i + 4] for i in range(0, len(words), 4))
        ]
        return iter(segments), SimpleNamespace(language=self._language)


class _EdgeModel(_PositionalModel):
    """Behaves like whisper at the end of an input that stops mid-utterance:
    a word straddling the end keeps only the heard part of its text, and a
    word starting in the last 0.95 s is left out. The last input, which
    reaches the end of the audio, is heard whole. Every other call is
    capitalised with trailing commas, so a re-decode never matches exactly."""

    onset = 0.5
    duration = 0.7

    def _word(self, t: int, start_s: float, end_s: float) -> _Word | None:
        onset = t + self.onset
        text = f" w{t}"
        cut_short = end_s * RATE < len(self._pcm)
        if cut_short and onset + self.duration > end_s:
            heard = (end_s - onset) / self.duration
            text = text[: max(2, round(len(text) * heard))]
        elif cut_short and onset >= end_s - 0.95:
            return None
        if len(self.calls) % 2 == 0:
            text = text.upper() + ","
        return _Word(text, onset - start_s, min(onset + self.duration, end_s) - start_s)


async def _run_positional(plan, *, chunk_seconds=0.5, language="en", model=None, **config):
    pcm = _speech_pcm(plan)
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = (model or _PositionalModel)(pcm)
    done: list[bool] = []
    events: list[tuple[bool, object]] = []

    async def emit(event):
        events.append((bool(done), event))

    cfg = SessionConfig(audio_format=FORMAT, language=language, **config)
    await adapter.run_session(cfg, _chunks(pcm, chunk_seconds, done), emit)
    return adapter._model, events


def _labels(plan, onset=0.1) -> list[str]:
    pcm = _speech_pcm(plan)
    return [f"w{t}" for t in range(len(pcm) // RATE) if pcm[round((t + onset) * RATE)] != 0]


def _plain(text: str) -> list[str]:
    return [w.strip(",").lower() for w in text.split()]


@pytest.fixture
def retained(monkeypatch) -> list[int]:
    """High-water mark of the shared window's raw PCM, in bytes."""
    from myna.testbed.streaming import loop as loop_module

    high_water = [0]
    base = loop_module.RollingWindow

    class _Spy(base):  # type: ignore[misc, valid-type]
        def fill(self, pcm):
            taken = super().fill(pcm)
            high_water[0] = max(high_water[0], len(self._buf))
            return taken

    monkeypatch.setattr(loop_module, "RollingWindow", _Spy)
    return high_water


@pytest.mark.parametrize("seconds", [130.0, 250.0])
async def test_batch_decodes_a_long_utterance_in_bounded_regions(retained, seconds):
    plan = [(seconds, True)]
    model, events = await _run_positional(plan)

    assert max(call["samples"] for call in model.calls) <= 65 * RATE
    assert 0 < retained[0] <= 65 * RATE * 2
    final_events = [e for _, e in events if isinstance(e, TranscriptionFinal)]
    assert "".join(e.text for e in final_events).split() == _labels(plan)
    assert events[-1][1] == TranscriptionDone(text="".join(e.text for e in final_events))


async def test_batch_presents_nothing_until_the_audio_has_ended():
    _, events = await _run_positional([(130.0, True)])

    shown = [(ended, e) for ended, e in events if not isinstance(e, TranscriptionProgress)]
    assert shown and all(ended for ended, _ in shown)
    assert all(e.snippet is None for _, e in events if isinstance(e, TranscriptionProgress))
    terminal = [e for _, e in events if isinstance(e, (TranscriptionDone, TranscriptionError))]
    assert terminal == [events[-1][1]]


async def test_batch_long_silence_stays_bounded_and_says_nothing(retained):
    pcm = np.zeros(200 * RATE, np.int16)
    model = _FakeWhisperModel()
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = model
    events = []

    async def emit(event):
        events.append(event)

    await adapter.run_session(SessionConfig(audio_format=FORMAT), _chunks(pcm, 1.0, []), emit)

    assert events[-1] == TranscriptionDone(text="")
    assert not finals(events)
    assert max(call["samples"] for call in model.calls) <= 65 * RATE
    assert 0 < retained[0] <= 65 * RATE * 2


async def test_batch_decodes_the_shortest_input():
    adapter = adapter_with(_Segment(" hi"))
    events = []

    async def emit(event):
        events.append(event)

    async def blip():
        yield PcmChunk(data=b"\x01\x00" * 160, format=FORMAT)

    await adapter.run_session(SessionConfig(audio_format=FORMAT), blip(), emit)

    assert [call["samples"] for call in adapter._model.calls] == [160]
    assert events[-1] == TranscriptionDone(text="hi")


async def test_batch_speech_across_pause_cuts_resumes_at_each_cut():
    plan = [(31.0, True), (1.0, False), (31.0, True), (1.0, False), (5.0, True)]
    model, events = await _run_positional(plan, chunk_seconds=0.1)

    regions = [(c["first"], c["samples"]) for c in model.calls]
    assert len(regions) == 3, regions
    assert all(first + n == regions[i + 1][0] for i, (first, n) in enumerate(regions[:-1]))
    assert not any(c.get("word_timestamps") for c in model.calls)
    text = "".join(e.text for _, e in events if isinstance(e, TranscriptionFinal))
    assert text.split() == _labels(plan)
    assert "  " not in text and not text.startswith(" ")


async def test_batch_forced_cuts_keep_every_word_once_with_absolute_timestamps():
    plan = [(130.0, True)]
    model, events = await _run_positional(plan, timestamp_granularity="word")

    assert len(model.calls) == 3
    final_events = [e for _, e in events if isinstance(e, TranscriptionFinal)]
    stamps = [s for e in final_events for s in e.segments]
    assert [s.text for s in stamps] == _labels(plan)
    for s in stamps:
        assert s.start == pytest.approx(int(s.text[1:]) + 0.1)
    assert [s.start for s in stamps] == sorted(s.start for s in stamps)


@pytest.mark.parametrize("granularity", [None, "word"])
async def test_batch_forced_cuts_keep_every_word_once_from_a_model_that_misses_the_edge(
    granularity,
):
    plan = [(130.0, True)]
    model, events = await _run_positional(plan, model=_EdgeModel, timestamp_granularity=granularity)

    assert len(model.calls) == 3
    final_events = [e for _, e in events if isinstance(e, TranscriptionFinal)]
    assert _plain("".join(e.text for e in final_events)) == _labels(plan, onset=0.5)
    if granularity:
        stamps = [s for e in final_events for s in e.segments]
        assert [s.text.strip(",").lower() for s in stamps] == _labels(plan, onset=0.5)


async def test_batch_segment_timestamps_span_the_words_the_final_carries():
    _, events = await _run_positional([(130.0, True)], timestamp_granularity="segment")

    for e in (e for _, e in events if isinstance(e, TranscriptionFinal)):
        (segment,) = e.segments
        words = e.text.split()
        assert segment.text == e.text
        assert segment.start == pytest.approx(int(words[0][1:]) + 0.1)
        assert segment.end == pytest.approx(int(words[-1][1:]) + 0.6)


async def test_batch_asks_for_word_alignment_only_around_forced_cuts():
    """Timestamps were not requested, so only the region ending at a forced
    cut (to hold back its last words) and the region re-decoding its overlap
    (to deduplicate) need aligned words."""
    model, _ = await _run_positional([(100.0, True), (1.0, False), (40.0, True)], chunk_seconds=0.1)

    assert [bool(c.get("word_timestamps")) for c in model.calls] == [True, True, False]


class _SkippingModel(_PositionalModel):
    """Whisper going quiet over part of a long region, as base does on the
    no-gaps stress clip: the words are simply absent from the decode, and the
    same audio nudged by a pad transcribes whole."""

    skip = range(40, 50)

    def transcribe(self, samples, **kwargs):
        self.nudged = not samples[0]
        return super().transcribe(samples, **kwargs)

    def _word(self, t: int, start_s: float, end_s: float):
        if t in self.skip and not self.nudged:
            return None
        return super()._word(t, start_s, end_s)


async def test_batch_re_decodes_a_region_that_left_speech_untranscribed():
    plan = [(70.0, True)]
    model, events = await _run_positional(plan, model=_SkippingModel)

    committed = [e for _, e in events if isinstance(e, TranscriptionFinal)]
    assert "".join(e.text for e in committed).split() == _labels(plan)
    # The first region (up to the forced cut) is decoded twice, the second once.
    assert [c["samples"] for c in model.calls][:2] == [60 * RATE, 60 * RATE + 2 * round(0.2 * RATE)]
    assert len(model.calls) == 3


async def test_batch_does_not_re_decode_a_region_it_transcribed():
    plan = [(70.0, True)]
    model, _ = await _run_positional(plan)

    assert len(model.calls) == 2, "a healthy region must not pay for a re-decode"


async def test_batch_keeps_the_language_it_detected_first():
    model, _ = await _run_positional([(130.0, True)], language=None)

    assert [c["language"] for c in model.calls] == [None, "en", "en"]


async def test_batch_carries_the_committed_context_across_a_cut():
    """faster-whisper conditions each 30 s window on up to 223 previous
    tokens; a cut must not reset that. Like faster-whisper, the context is
    the text emitted so far: a word the overlap decodes again is not in it
    twice, and a word held back for the next region is not in it yet."""
    model, _ = await _run_positional([(130.0, True)])

    assert model.calls[0]["initial_prompt"] is None
    assert model.calls[1]["initial_prompt"] == list(range(0, 59))
    assert model.calls[2]["initial_prompt"] == list(range(0, 118))


async def test_batch_context_starts_with_the_prompt_and_is_bounded():
    class _Tokenizer(_WordTokenizer):
        def encode(self, text, add_special_tokens=True):
            if text == " Myna":
                return SimpleNamespace(ids=[-5] * 200)
            return super().encode(text, add_special_tokens)

    plan = [(250.0, True)]
    pcm = _speech_pcm(plan)
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _PositionalModel(pcm)
    adapter._model.hf_tokenizer = _Tokenizer()

    async def emit(_event):
        pass

    cfg = SessionConfig(audio_format=FORMAT, language="en", prompt="Myna")
    await adapter.run_session(cfg, _chunks(pcm, 1.0, []), emit)

    prompts = [c["initial_prompt"] for c in adapter._model.calls]
    assert len(prompts) == 5
    assert prompts[0] == "Myna"
    history = [-5] * 200
    for k, end in enumerate((59, 118, 177, 236)):
        history += list(range(len(history) - 200, end))
        assert prompts[k + 1] == history[-223:]


async def test_batch_unaligned_segments_are_timed_in_absolute_seconds():
    """Alignment can come back empty; the segment span it degrades to must
    still be offset to where its region sits in the utterance."""

    class _Unaligned(_PositionalModel):
        def transcribe(self, samples, **kwargs):
            segments, info = super().transcribe(samples, **kwargs)
            stripped = []
            for segment in segments:
                segment.words = None
                stripped.append(segment)
            return iter(stripped), info

    plan = [(31.0, True), (1.0, False), (98.0, True)]
    pcm = _speech_pcm(plan)
    adapter = FasterWhisperAdapter("tiny")
    adapter._model = _Unaligned(pcm)
    events = []

    async def emit(event):
        events.append(event)

    cfg = SessionConfig(audio_format=FORMAT, language="en", timestamp_granularity="word")
    await adapter.run_session(cfg, _chunks(pcm, 0.1, []), emit)

    assert len(adapter._model.calls) == 3, "a pause cut, then a forced cut"
    for final in finals(events):
        (segment,) = final.segments
        words = final.text.split()
        if len(words) == 4:  # unaligned, a trimmed segment can only keep its span
            assert segment.start == pytest.approx(int(words[0][1:]) + 0.1)
            assert segment.end == pytest.approx(int(words[-1][1:]) + 0.6)
    assert "".join(f.text for f in finals(events)).split() == _labels(plan)
