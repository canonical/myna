"""The Nemotron adapter's own logic, without ONNX Runtime or a model:
incremental mel, chunk stepping with pad-and-drop, greedy RNN-T, the text,
and the session's events. The real graphs are checked against NeMo by
``dev/nemotron/parity.py``, and the adapter end to end against that."""

import pytest
from hypothesis import given, settings
from hypothesis import strategies as st

np = pytest.importorskip("numpy", reason="adapter extras not installed")

from myna.core import (  # noqa: E402
    PHASE_PREPARING,
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
from myna.testbed import nemotron  # noqa: E402
from myna.testbed.nemotron import (  # noqa: E402
    FORMAT,
    MAX_SYMBOLS,
    Decoder,
    MelStream,
    NemotronAdapter,
    Shape,
    Stream,
    detokenize,
    load_vocab,
)

HOP, N_FFT = 4, 10


def toy_mel(samples):
    """A centred, zero-padded framing with a pre-emphasis tap, shaped like
    the real graph: frame k sums the filtered window around sample k * HOP."""
    filtered = np.concatenate([samples[:1], samples[1:] - 0.97 * samples[:-1]])
    padded = np.pad(filtered, N_FFT // 2)
    frames = len(samples) // HOP + 1
    out = np.array(
        [
            [padded[k * HOP : k * HOP + N_FFT].sum(), padded[k * HOP : k * HOP + N_FFT].max()]
            for k in range(frames)
        ],
        np.float32,
    )
    return out.T


@settings(max_examples=60, deadline=None)
@given(
    pieces=st.lists(st.integers(1, 40), min_size=1, max_size=30),
    seed=st.integers(0, 2**16),
)
def test_mel_frames_match_a_pass_over_the_whole_signal(pieces, seed):
    signal = np.random.default_rng(seed).standard_normal(sum(pieces)).astype(np.float32)
    mel = MelStream(toy_mel, HOP, N_FFT)
    at = 0
    for n in pieces:
        mel.push(signal[at : at + n])
        at += n
    whole = toy_mel(signal)
    complete = (len(signal) - N_FFT // 2) // HOP + 1 if len(signal) >= N_FFT // 2 else 0
    assert mel.count == complete
    np.testing.assert_allclose(
        mel.frames(0, mel.count, 2), whole[:, :complete], rtol=1e-6, atol=1e-6
    )


def test_frames_before_the_signal_are_zeros_and_released_frames_are_gone():
    mel = MelStream(toy_mel, HOP, N_FFT)
    mel.push(np.ones(100, np.float32))
    got = mel.frames(-3, 2, 2)
    assert got.shape == (2, 5)
    assert not got[:, :3].any()
    np.testing.assert_array_equal(got[:, 3:], mel.frames(0, 2, 2))
    kept = mel.frames(10, 12, 2)
    mel.release(10)
    assert mel.origin == 10
    np.testing.assert_array_equal(mel.frames(10, 12, 2), kept)


def test_frames_before_any_audio_are_all_zeros():
    assert MelStream(toy_mel, HOP, N_FFT).frames(-4, 0, 2).shape == (2, 4)


def test_shape_takes_the_steady_chunk_from_the_config():
    shape = Shape.from_config(
        {
            "streaming": {"chunk_size": [49, 56], "pre_encode_cache_size": [0, 9]},
            "preprocessor": {"features": 128, "window_stride": 0.01, "n_fft": 512},
        }
    )
    assert shape == Shape(chunk=56, pre_cache=9, features=128, hop=160, n_fft=512)
    flat = Shape.from_config(
        {
            "streaming": {"chunk_size": 8, "pre_encode_cache_size": 3},
            "preprocessor": {"features": 2, "window_stride": 0.01, "n_fft": 512},
        }
    )
    assert (flat.chunk, flat.pre_cache) == (8, 3)


BLANK = 3


def scripted_joint(script):
    """A joint that returns the next token of ``script`` as its argmax and
    records what it was asked; the state counts emissions."""
    calls = []

    def joint(frame, prev, state):
        calls.append((float(frame[0, 0, 0]), prev, float(state[0][0, 0, 0])))
        logits = np.zeros(BLANK + 1, np.float32)
        logits[script.pop(0)] = 1.0
        return logits, (state[0] + 1, state[1])

    return joint, calls


def fresh_decoder():
    return Decoder(
        blank=BLANK, state=(np.zeros((1, 1, 1), np.float32), np.zeros((1, 1, 1), np.float32))
    )


def test_greedy_emits_until_blank_and_only_emissions_advance_the_prediction_net():
    decoder = fresh_decoder()
    joint, calls = scripted_joint([0, 1, BLANK, BLANK, 2, BLANK])
    frames = np.array([[10.0, 20.0, 30.0]], np.float32)
    assert decoder.decode(frames, joint) == [0, 1, 2]
    # frame, previous token, emissions so far
    assert calls == [
        (10.0, BLANK, 0.0),
        (10.0, 0, 1.0),
        (10.0, 1, 2.0),
        (20.0, 1, 2.0),
        (30.0, 1, 2.0),
        (30.0, 2, 3.0),
    ]


def test_greedy_carries_its_state_into_the_next_step():
    decoder = fresh_decoder()
    joint, _ = scripted_joint([2, BLANK])
    decoder.decode(np.zeros((1, 1), np.float32), joint)
    joint, calls = scripted_joint([BLANK])
    decoder.decode(np.ones((1, 1), np.float32), joint)
    assert calls == [(1.0, 2, 1.0)]


def test_greedy_moves_on_after_max_symbols_at_one_frame():
    decoder = fresh_decoder()
    joint, _ = scripted_joint([0] * MAX_SYMBOLS + [BLANK])
    assert decoder.decode(np.zeros((1, 2), np.float32), joint) == [0] * MAX_SYMBOLS


def test_vocab_and_text(tmp_path):
    (tmp_path / "vocab.txt").write_text(
        "<unk> 0\n▁hel 1\nlo 2\n▁<fr-FR> 3\n. 4\n<blk> 5\n", encoding="utf-8"
    )
    pieces = load_vocab(tmp_path / "vocab.txt")
    assert pieces[1] == "▁hel" and pieces[-1] == "<blk>"
    assert detokenize(pieces, [1, 2, 4, 3]) == " hello."
    assert detokenize(pieces, [1, 4, 3, 1]) == " hel. hel"
    assert detokenize(pieces, []) == ""


class FakeEncoder:
    """Echoes the newest chunk's frames back through an 8x-free 'encoder'
    and counts steps in its cache, so a test can see what each step got."""

    def __init__(self, shape):
        self.shape = shape
        self.inputs = []

    def run(self, _names, feed):
        x = feed["audio_signal"]
        self.inputs.append(
            (x.copy(), {k: v.copy() for k, v in feed.items() if k.startswith("cache")})
        )
        assert feed["length"][0] == x.shape[-1] == self.shape.pre_cache + self.shape.chunk
        encoded = x[:, :, self.shape.pre_cache :]
        return [
            encoded,
            np.array([encoded.shape[-1]]),
            feed["cache_last_channel"] + 1,
            feed["cache_last_time"],
            feed["cache_last_channel_len"] + 1,
        ]


class FakeModel:
    def __init__(self):
        self.shape = Shape(chunk=3, pre_cache=2, features=1, hop=HOP, n_fft=N_FFT)
        self.pieces = ["▁a", "b", "c", "<blk>"]
        self.blank = 3
        self.state_width = 1
        self.cache_shapes = {
            "cache_last_channel": [1, 1, 1, 1],
            "cache_last_time": [1, 1, 1, 1],
            "cache_last_channel_len": [1],
        }
        self.encoder = FakeEncoder(self.shape)
        self.prompt = None
        self.emitted = set()

    def log_mel(self, samples):
        return np.arange(len(samples) // HOP + 1, dtype=np.float32)[None] + 1

    def joint_step(self, frame, prev, state):
        # Emit "a" once per frame, then blank.
        value = float(frame[0, 0, 0])
        logits = np.zeros(4, np.float32)
        logits[3 if value in self.emitted else 0] = 1.0
        self.emitted.add(value)
        return logits, state


def test_a_stream_steps_each_full_chunk_after_a_zero_pre_cache():
    model = FakeModel()
    stream = Stream(model)  # type: ignore[arg-type]
    text = stream.feed(np.zeros(8 * HOP, np.float32))  # 7 complete frames: two chunks
    steps = model.encoder.inputs
    assert len(steps) == 2
    first, second = steps[0][0][0, 0], steps[1][0][0, 0]
    np.testing.assert_array_equal(first, [0, 0, 1, 2, 3])  # zeros, then frames 0-2
    np.testing.assert_array_equal(second, [2, 3, 4, 5, 6])  # frames 1-5
    assert steps[1][1]["cache_last_channel_len"][0] == 1  # the first step's caches came back in
    assert text == " a a a a a a"
    assert stream.finish().startswith(" a")


def test_a_stream_holds_back_a_partial_chunk():
    model = FakeModel()
    stream = Stream(model)  # type: ignore[arg-type]
    assert stream.feed(np.zeros(3 * HOP, np.float32)) == ""
    assert model.encoder.inputs == []


# --- the session -------------------------------------------------------------


class ScriptedStream:
    """Replays texts: one per ``feed``, the last for ``finish``."""

    texts: list[str] = []
    fail = False

    def __init__(self, model, prompt=None):
        self.model, self.prompt = model, prompt

    def feed(self, samples):
        if ScriptedStream.fail:
            raise RuntimeError("encoder exploded")
        assert samples.dtype == np.float32
        return ScriptedStream.texts.pop(0)

    def finish(self):
        return ScriptedStream.texts.pop(0)


class LoadedModel:
    prompts: dict[str, int] = {}

    def prompt_vector(self, language):
        return language


@pytest.fixture
def adapter(monkeypatch):
    monkeypatch.setattr(nemotron, "Stream", ScriptedStream)
    ScriptedStream.fail = False
    a = NemotronAdapter("/nowhere/nemotron-en-rc6")

    async def load():
        a._model = LoadedModel()  # type: ignore[assignment]
        return a._model

    monkeypatch.setattr(a, "_load_model", load)
    return a


async def run(adapter, chunks=2, fmt=FORMAT):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for _ in range(chunks):
            yield PcmChunk(data=b"\x00\x40" * 160, format=fmt)

    await adapter.run_session(SessionConfig(audio_format=fmt), audio(), emit)
    return events


async def test_every_step_commits_and_done_is_their_concatenation(adapter):
    ScriptedStream.texts = [" Hello", "", " world."]
    events = await run(adapter)
    assert [type(e) for e in events[:2]] == [TranscriptionProgress, TranscriptionProgress]
    assert (events[0].phase, events[1].phase) == (PHASE_PREPARING, PHASE_READY)
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [(f.text, f.segment_index, f.disposition) for f in finals] == [
        ("Hello", 0, Disposition.COMMITTED),
        (" world.", 1, Disposition.COMMITTED),
    ]
    assert events[-1] == TranscriptionDone(text="Hello world.")


async def test_leading_space_only_text_is_not_a_commit(adapter):
    ScriptedStream.texts = [" ", " Hi", ""]
    events = await run(adapter)
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [(f.text, f.segment_index) for f in finals] == [("Hi", 0)]
    assert events[-1] == TranscriptionDone(text="Hi")


async def test_silence_ends_with_an_empty_done(adapter):
    ScriptedStream.texts = ["", "", ""]
    events = await run(adapter)
    assert not [e for e in events if isinstance(e, TranscriptionFinal)]
    assert events[-1] == TranscriptionDone(text="")


async def test_a_failing_step_is_an_inference_error(adapter):
    ScriptedStream.fail = True
    events = await run(adapter)
    assert isinstance(events[-1], TranscriptionError)
    assert events[-1].code == "inference_failed"
    assert "encoder exploded" in events[-1].message


@pytest.mark.parametrize(
    "fmt",
    [
        AudioFormat(sample_rate_hz=48_000, channels=1, sample_width_bytes=2),
        AudioFormat(sample_rate_hz=16_000, channels=2, sample_width_bytes=2),
        AudioFormat(sample_rate_hz=16_000, channels=1, sample_width_bytes=4),
    ],
)
async def test_other_formats_are_refused_before_loading(adapter, fmt):
    events = await run(adapter, fmt=fmt)
    assert len(events) == 1
    assert events[0].code == "unsupported_audio_format"
    assert adapter._model is None


async def test_capabilities_and_candidate(adapter):
    caps = adapter.capabilities()
    assert caps.streaming and caps.punctuation and not caps.translation
    assert caps.languages == ("en",)
    assert caps.models == ("nemotron-en-rc6",)
    assert adapter.candidate.streaming_strategy == "native-transducer"
    assert adapter.streaming


async def test_a_multilingual_model_lists_the_languages_it_has_prompts_for(adapter):
    ScriptedStream.texts = ["", ""]
    await run(adapter, chunks=1)
    adapter._model.prompts = {"en-US": 0, "fr-FR": 1, "xx-YY": 2}
    assert adapter.capabilities().languages == ("en", "fr")


async def test_unload_forgets_the_model(adapter):
    ScriptedStream.texts = ["", ""]
    await run(adapter, chunks=1)
    await adapter.unload()
    assert adapter._model is None


def test_the_model_dir_defaults_to_the_environment(monkeypatch):
    monkeypatch.setenv("MODEL_DIR", "/snap/x/model")
    assert NemotronAdapter().candidate.model == "model"
    monkeypatch.delenv("MODEL_DIR")
    assert NemotronAdapter().candidate.model == "nemotron-en-rc6"


def test_the_language_prompt(tmp_path):
    model = object.__new__(nemotron._Model)

    class Input:
        shape = [1, 4]

    class Prompt:
        def get_inputs(self):
            return [Input(), Input()]

    model.prompt = None
    assert model.prompt_vector("fr") is None
    model.prompt = Prompt()
    model.prompts = {"auto": 0, "en-US": 1, "fr-FR": 2}
    assert int(np.argmax(model.prompt_vector("fr"))) == 2
    assert int(np.argmax(model.prompt_vector("fr-FR"))) == 2
    assert int(np.argmax(model.prompt_vector("zz"))) == 0
    assert int(np.argmax(model.prompt_vector(None))) == 0
