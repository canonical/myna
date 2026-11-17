"""The IndicConformer adapter's own logic, without ONNX Runtime or a model:
the log-mel, both greedy decoders, word grouping and the session's events.
The real graphs are checked against NeMo by
``dev/indicconformer/parity.py``."""

import pytest

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
)
from myna.testbed import indicconformer  # noqa: E402
from myna.testbed.indicconformer import (  # noqa: E402
    FORMAT,
    IndicConformerAdapter,
    MelSpec,
    ctc_greedy,
    load_vocab,
    log_mel,
    to_words,
)
from myna.testbed.streaming.strategies import Word  # noqa: E402

N_FFT, HOP = 16, 4


def filterbank(fb):
    return lambda power: np.log(fb @ power + 2**-24).astype(np.float32)


def spec(features=3, preemph=0.97, fb=None):
    if fb is None:
        fb = np.random.default_rng(0).random((features, N_FFT // 2 + 1)).astype(np.float32)
    return MelSpec(filterbank=filterbank(fb), win_length=12, n_fft=N_FFT, hop=HOP, preemph=preemph)


def test_mel_counts_frames_as_nemo_does_and_zeroes_the_extra_one():
    samples = np.random.default_rng(1).standard_normal(103).astype(np.float32)
    mel, valid = log_mel(samples, spec())
    assert valid == 103 // HOP
    assert mel.shape == (3, valid + 1)
    assert not mel[:, valid:].any()


def test_mel_normalises_each_feature_over_the_valid_frames():
    samples = np.random.default_rng(2).standard_normal(400).astype(np.float32)
    mel, valid = log_mel(samples, spec())
    body = mel[:, :valid]
    np.testing.assert_allclose(body.mean(axis=1), 0.0, atol=1e-5)
    np.testing.assert_allclose(body.std(axis=1, ddof=1), 1.0, atol=1e-3)


def test_mel_centres_a_short_window_in_the_fft_frame():
    """torch.stft centres a 12-sample window in the 16-sample frame, so an
    impulse reaches the frames centred within 6 samples of it."""
    flat = spec(preemph=0.0, fb=np.ones((1, N_FFT // 2 + 1), np.float32))
    samples = np.zeros(40, np.float32)
    samples[3 * HOP] = 1.0
    mel, valid = log_mel(samples, flat)
    hit = mel[0, :valid] > mel[0, :valid].min()
    # Frame k is centred on sample 4k: frames 2, 3 and 4 (centres 8, 12, 16).
    assert hit.tolist() == [False, False, True, True, True, False, False, False, False, False]


def test_mel_takes_its_settings_from_the_config():
    config = {
        "preprocessor": {
            "n_fft": 512, "hop": 160, "win_length": 400, "preemph": 0.97,
            "normalize": "per_feature",
        }
    }  # fmt: skip
    got = MelSpec.load(config, np.log)
    assert (got.n_fft, got.hop, got.win_length, got.preemph) == (512, 160, 400, 0.97)
    config["preprocessor"]["normalize"] = "all_features"
    with pytest.raises(ValueError, match="all_features"):
        MelSpec.load(config, np.log)


def test_mel_of_less_than_a_hop_is_all_zeros():
    mel, valid = log_mel(np.ones(3, np.float32), spec())
    assert valid == 0
    assert mel.shape == (3, 1) and not mel.any()


def test_mel_of_one_valid_frame_is_finite():
    mel, valid = log_mel(np.ones(HOP, np.float32), spec())
    assert valid == 1 and np.isfinite(mel).all()


BLANK = 4


def frames_of(best):
    """Log-probs whose frame-wise argmax is ``best``."""
    out = np.full((len(best), BLANK + 1), -10.0, np.float32)
    out[np.arange(len(best)), best] = 0.0
    return out


def test_ctc_collapses_repeats_and_drops_blanks():
    best = [BLANK, 1, 1, BLANK, 1, 2, 2, 3, BLANK]
    assert ctc_greedy(frames_of(best), BLANK) == [(1, 1), (1, 4), (2, 5), (3, 7)]


def test_ctc_of_blanks_is_nothing():
    assert ctc_greedy(frames_of([BLANK] * 5), BLANK) == []


def test_vocab_turns_word_marks_into_spaces(tmp_path):
    (tmp_path / "vocab.txt").write_text("<unk> 0\n▁क 1\nा 2\n<blk> 3\n", encoding="utf-8")
    assert load_vocab(tmp_path / "vocab.txt") == ["<unk>", " क", "ा", "<blk>"]


def test_words_start_at_a_space_and_keep_it():
    pieces = ["<unk>", " नम", "स्", "ते", " दु", "निया"]
    words = to_words(pieces, [(1, 0), (2, 1), (3, 2), (4, 5), (5, 6)], 0.04, 10.0)
    assert words == [
        Word(" नमस्ते", 10.0, pytest.approx(10.12)),
        Word(" दुनिया", pytest.approx(10.2), pytest.approx(10.28)),
    ]


def test_a_leading_continuation_piece_starts_a_word_and_blank_words_go():
    pieces = ["x", "ा", " ", " क"]
    assert [w.text for w in to_words(pieces, [(1, 0), (2, 1)], 1.0, 0.0)] == ["ा"]
    words = to_words(pieces, [(1, 0), (2, 1), (3, 2), (0, 3)], 1.0, 0.0)
    assert [w.text for w in words] == ["ा", " कx"]


class ScriptedJoint:
    """A decoder_joint replaying tokens; records (frame, prev, state) per call."""

    def __init__(self, tokens):
        self.tokens = list(tokens)
        self.calls = []

    def run(self, _outputs, feeds):
        frame = float(feeds["encoder_outputs"][0, 0, 0])
        prev = int(feeds["targets"][0, 0])
        state = float(feeds["input_states_1"].ravel()[0])
        self.calls.append((frame, prev, state))
        token = self.tokens.pop(0)
        logits = np.full((1, 1, 1, BLANK + 1), -1.0, np.float32)
        logits[..., token] = 0.0
        nxt = np.full_like(feeds["input_states_1"], state + 1)
        return logits, np.array([1]), nxt, nxt


def rnnt_model(tokens, max_symbols=10):
    model = object.__new__(indicconformer._Model)
    model.joint = ScriptedJoint(tokens)
    model.blank = BLANK
    model.max_symbols = max_symbols
    model.state_shape = (2, 1, 3)
    return model


def test_rnnt_emits_until_blank_and_only_emissions_advance_the_prediction_net():
    model = rnnt_model([0, 1, BLANK, BLANK, 2, BLANK])
    frames = np.array([[10.0, 20.0, 30.0]], np.float32)
    assert model._rnnt(frames) == [(0, 0), (1, 0), (2, 2)]
    assert model.joint.calls == [
        (10.0, BLANK, 0.0),
        (10.0, 0, 1.0),
        (10.0, 1, 2.0),
        (20.0, 1, 2.0),
        (30.0, 1, 2.0),
        (30.0, 2, 3.0),
    ]


def test_rnnt_moves_on_after_max_symbols_at_one_frame():
    model = rnnt_model([0, 0, 0, BLANK], max_symbols=3)
    assert model._rnnt(np.zeros((1, 2), np.float32)) == [(0, 0), (0, 0), (0, 0)]


class FakeSession:
    """An ORT session over a path, running ``fn`` on its feeds."""

    def __init__(self, path, opts=None, providers=None, fn=None, inputs=None):
        self.path, self.opts, self.providers = str(path), opts, providers
        self.fn, self.inputs = fn, inputs or []

    def run(self, _outputs, feeds):
        return self.fn(feeds)

    def get_inputs(self):
        return self.inputs


class FakeOrt:
    """Just enough onnxruntime for ``_Model``: records every session."""

    def __init__(self):
        self.sessions = []

    class SessionOptions:
        intra_op_num_threads = 0
        inter_op_num_threads = 0

    def InferenceSession(self, path, opts, providers):  # noqa: N802
        class Input:
            shape = [2, "batch", 640]

        session = FakeSession(path, opts, providers, inputs=[None, None, None, Input()])
        self.sessions.append(session)
        return session


def model_dir(tmp_path, *graphs):
    tmp_path.mkdir(exist_ok=True)
    (tmp_path / "config.json").write_text(
        '{"preprocessor": {"n_fft": 16, "hop": 4, "win_length": 12, "preemph": 0.97,'
        ' "normalize": "per_feature"}, "subsampling_factor": 4, "max_symbols": 7}'
    )
    (tmp_path / "vocab.txt").write_text("<unk> 0\n▁a 1\n<blk> 2\n", encoding="utf-8")
    for name in graphs:
        (tmp_path / name).write_bytes(b"")
    return tmp_path


def test_the_model_prefers_int8_graphs_and_loads_the_head_it_serves(tmp_path, monkeypatch):
    ort = FakeOrt()
    monkeypatch.setitem(__import__("sys").modules, "onnxruntime", ort)
    d = model_dir(tmp_path, "encoder-model.int8.onnx", "encoder-model.onnx")
    model = indicconformer._Model(d, "ctc", 3)
    names = [s.path.rsplit("/", 1)[1] for s in ort.sessions]
    assert names == ["mel-model.onnx", "encoder-model.int8.onnx", "ctc-model.onnx"]
    assert [s.opts.intra_op_num_threads for s in ort.sessions] == [1, 3, 1]
    assert {s.opts.inter_op_num_threads for s in ort.sessions} == {1}
    assert (model.precision, model.blank, model.max_symbols) == ("int8", 2, 7)
    assert model.frame_s == pytest.approx(4 * 4 / 16_000)

    ort.sessions.clear()
    model = indicconformer._Model(model_dir(tmp_path / "fp32"), "rnnt", 3)
    names = [s.path.rsplit("/", 1)[1] for s in ort.sessions]
    assert names == ["mel-model.onnx", "encoder-model.onnx", "decoder_joint-model.onnx"]
    assert model.precision == "fp32"
    assert model.state_shape == (2, 1, 640)


def test_transcription_runs_mel_encoder_and_head_with_valid_lengths():
    model = object.__new__(indicconformer._Model)
    seen = {}

    def encoder(feeds):
        seen["length"] = int(feeds["length"][0])
        seen["frames"] = feeds["audio_signal"].shape[-1]
        encoded = np.zeros((1, 2, 5), np.float32)
        return encoded, np.array([3])

    def ctc(feeds):
        seen["head"] = feeds["encoder_outputs"].shape
        return (frames_of([BLANK, 1, BLANK])[None],)

    model.filterbank = FakeSession("mel", fn=lambda feeds: (np.log(feeds["power"][:, :3] + 1),))
    model.mel = MelSpec(model._log_mel, 12, N_FFT, HOP, 0.97)
    model.encoder = FakeSession("enc", fn=encoder)
    model.ctc = FakeSession("ctc", fn=ctc)
    model.decoder = "ctc"
    model.blank = BLANK
    model.pieces = ["a", " b", "c", "d", "<blk>"]
    model.frame_s = 0.04
    words = model.transcribe_words(np.ones(40, np.float32), offset=2.0)
    assert seen == {"length": 10, "frames": 11, "head": (1, 2, 3)}
    assert words == [Word(" b", 2.04, pytest.approx(2.08))]

    model.decoder = "rnnt"
    model._rnnt = lambda frames: [(2, 0)]
    assert [w.text for w in model.transcribe_words(np.ones(40, np.float32))] == ["c"]


async def test_the_model_loads_once_off_the_event_loop(tmp_path, monkeypatch):
    built = []

    def fake_model(*args):
        built.append(args)
        return LoadedModel()

    monkeypatch.setattr(indicconformer, "_Model", fake_model)
    a = IndicConformerAdapter(str(tmp_path / "indicconformer-large-hi"), encoder_threads=2)
    first = await a._load_model()
    assert await a._load_model() is first
    assert built == [(tmp_path / "indicconformer-large-hi", "ctc", 2)]


class LoadedModel:
    """Replays one word list per decode; ``fail`` raises instead."""

    def __init__(self):
        self.decodes = []
        self.texts = []
        self.fail = False

    def transcribe_words(self, samples, offset=0.0):
        if self.fail:
            raise RuntimeError("encoder exploded")
        assert samples.dtype == np.float32
        self.decodes.append(len(samples))
        text = self.texts.pop(0) if self.texts else ""
        return [Word(t, offset, offset + 0.1) for t in text.split("|") if t]


@pytest.fixture
def adapter(monkeypatch):
    a = IndicConformerAdapter("/nowhere/indicconformer-large-hi")
    model = LoadedModel()

    async def load():
        a._model = model  # type: ignore[assignment]
        return model

    monkeypatch.setattr(a, "_load_model", load)
    return a, model


async def run(adapter, seconds=1.0, fmt=FORMAT):
    events = []

    async def emit(event):
        events.append(event)

    async def audio():
        for _ in range(int(seconds * 10)):
            yield PcmChunk(data=b"\x00\x40" * 1600, format=fmt)

    await adapter.run_session(SessionConfig(audio_format=fmt), audio(), emit)
    return events


async def test_batch_is_one_final_of_the_decoded_words(adapter):
    a, model = adapter
    model.texts = [" नमस्ते| दुनिया"]
    events = await run(a)
    assert (events[0].phase, events[1].phase) == (PHASE_PREPARING, PHASE_READY)
    finals = [e for e in events if isinstance(e, TranscriptionFinal)]
    assert [(f.text, f.disposition) for f in finals] == [("नमस्ते दुनिया", Disposition.COMMITTED)]
    assert events[-1] == TranscriptionDone(text="नमस्ते दुनिया")
    assert model.decodes == [16_000]


async def test_batch_silence_ends_with_an_empty_done_and_no_final(adapter):
    a, _model = adapter
    events = await run(a)
    assert not [e for e in events if isinstance(e, TranscriptionFinal)]
    assert events[-1] == TranscriptionDone(text="")


async def test_streaming_commits_through_the_chunked_loop(adapter):
    a, model = adapter
    a._streaming = True
    model.texts = [" नमस्ते"] * 20
    events = await run(a)
    committed = [
        e for e in events
        if isinstance(e, TranscriptionFinal) and e.disposition == Disposition.COMMITTED
    ]  # fmt: skip
    assert [c.text for c in committed] == ["नमस्ते"]
    assert events[-1] == TranscriptionDone(text="नमस्ते")


async def test_a_failing_decode_is_an_inference_error(adapter):
    a, model = adapter
    model.fail = True
    events = await run(a)
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
    a, _model = adapter
    events = await run(a, fmt=fmt)
    assert len(events) == 1
    assert events[0].code == "unsupported_audio_format"
    assert a._model is None


def test_capabilities_name_the_model_dirs_language():
    a = IndicConformerAdapter("/snap/x/indicconformer-large-hi")
    caps = a.capabilities()
    assert caps.languages == ("hi",)
    assert caps.models == ("indicconformer-large-hi",)
    assert not caps.punctuation and not caps.streaming
    assert a.candidate.engine == "onnxruntime-cpu-ctc"
    assert a.candidate.streaming_strategy == "commit-on-finalize"
    streaming = IndicConformerAdapter("/m/indicconformer-large-hi", streaming=True, decoder="rnnt")
    assert streaming.streaming and streaming.capabilities().streaming
    assert streaming.candidate.engine == "onnxruntime-cpu-rnnt"
    assert streaming.candidate.streaming_strategy == "chunked-commit"


def test_an_unknown_decoder_is_refused():
    with pytest.raises(ValueError, match="rnnt or ctc"):
        IndicConformerAdapter("/m", decoder="beam")  # type: ignore[arg-type]


async def test_unload_forgets_the_model(adapter):
    a, _model = adapter
    await run(a, seconds=0.1)
    await a.unload()
    assert a._model is None


def test_the_model_dir_defaults_to_the_environment(monkeypatch):
    monkeypatch.setenv("MODEL_DIR", "/snap/x/indicconformer-large-hi")
    assert IndicConformerAdapter().candidate.model == "indicconformer-large-hi"
    monkeypatch.delenv("MODEL_DIR")
    assert IndicConformerAdapter().candidate.model == "indicconformer-large-hi"
