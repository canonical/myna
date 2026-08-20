"""Corpus subcommands: download-corpus and make-corpus.

Both produce the same manifest schema `myna.testbed.corpus.load_manifest`
consumes, so the tests assert against that loader rather than against the JSON
by eye: a manifest this code writes and the harness cannot read is the failure
worth catching, and it is invisible to a field-by-field comparison.

No network and no ffmpeg: the LibriSpeech tarball is synthesised in tmp_path
and the FLAC decode is the one seam stubbed out.
"""

from __future__ import annotations

import array
import io
import json
import subprocess
import tarfile
import wave

import pytest

from myna.benchmarker import _corpus
from myna.benchmarker._corpus import (
    RATE,
    _build_corpus,
    _read_wav_meta,
    _round_robin,
    _speaker,
    _write_wav,
    cmd_download,
    cmd_make,
)
from myna.testbed.corpus import load_manifest


def tone(seconds: float = 0.25, rate: int = RATE) -> array.array:
    return array.array("h", [(i % 400) - 200 for i in range(int(seconds * rate))])


def write_wav(path, seconds: float = 0.25, rate: int = RATE, channels: int = 1) -> None:
    with wave.open(str(path), "w") as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(2)
        wf.setframerate(rate)
        wf.writeframes(tone(seconds, rate).tobytes() * channels)


# ─── selection helpers ───────────────────────────────────────────────────────


def test_speaker_is_the_leading_id_segment():
    assert _speaker("84-121123-0000") == "84"


def test_speaker_of_an_id_without_separators_is_the_whole_id():
    assert _speaker("solo") == "solo"


def test_round_robin_takes_one_per_speaker_before_taking_a_second():
    picked = _round_robin({"1": ["a1", "a2", "a3"], "2": ["b1", "b2"], "3": ["c1"]}, 5)
    assert picked[:3] == ["a1", "b1", "c1"]
    assert set(picked[3:]) == {"a2", "b2"}


def test_round_robin_stops_at_n_mid_row():
    assert _round_robin({"1": ["a1", "a2"], "2": ["b1", "b2"]}, 3) == ["a1", "b1", "a2"]


def test_round_robin_stops_early_when_the_pool_is_exhausted():
    assert _round_robin({"1": ["a1"]}, 10) == ["a1"]


def test_round_robin_of_an_empty_pool_is_empty():
    assert _round_robin({}, 5) == []


# ─── WAV read/write ──────────────────────────────────────────────────────────


def test_write_wav_round_trips_through_read_wav_meta(tmp_path):
    path = tmp_path / "clip.wav"
    duration = _write_wav(path, tone(0.5), RATE)
    assert duration == pytest.approx(0.5)
    assert _read_wav_meta(path) == (0.5, RATE, 1)


def test_write_wav_produces_16_khz_mono_s16le(tmp_path):
    path = tmp_path / "clip.wav"
    _write_wav(path, tone(), RATE)
    with wave.open(str(path), "r") as wf:
        assert (wf.getnchannels(), wf.getsampwidth(), wf.getframerate()) == (1, 2, RATE)


def test_read_wav_meta_reports_stereo_and_odd_rates(tmp_path):
    path = tmp_path / "stereo.wav"
    write_wav(path, seconds=0.5, rate=8_000, channels=2)
    duration, rate, channels = _read_wav_meta(path)
    assert (rate, channels) == (8_000, 2)
    assert duration == pytest.approx(0.5)


def test_read_wav_meta_of_an_empty_clip_is_zero_seconds(tmp_path):
    path = tmp_path / "empty.wav"
    _write_wav(path, array.array("h"), RATE)
    assert _read_wav_meta(path)[0] == 0.0


# ─── _decode_flac ────────────────────────────────────────────────────────────


def test_decode_flac_pipes_the_bytes_through_ffmpeg_at_16k_mono(monkeypatch):
    seen = {}

    def run(cmd, input=None, **kwargs):  # noqa: A002
        seen["cmd"] = cmd
        seen["input"] = input
        return subprocess.CompletedProcess(cmd, 0, stdout=tone(0.1).tobytes())

    monkeypatch.setattr(_corpus.subprocess, "run", run)
    samples = _corpus._decode_flac(b"fake-flac")

    assert seen["input"] == b"fake-flac"
    assert seen["cmd"][0] == "ffmpeg"
    assert "-ar" in seen["cmd"] and str(RATE) in seen["cmd"]
    assert seen["cmd"][seen["cmd"].index("-ac") + 1] == "1"
    assert len(samples) == int(0.1 * RATE)


# ─── _build_corpus ───────────────────────────────────────────────────────────


def make_tarball(path, subset, utterances):
    """A minimal LibriSpeech-shaped tar.gz: .flac files plus .trans.txt."""
    by_chapter: dict[tuple[str, str], list[tuple[str, str]]] = {}
    with tarfile.open(path, "w:gz") as tar:

        def add(name: str, data: bytes) -> None:
            info = tarfile.TarInfo(name)
            info.size = len(data)
            tar.addfile(info, io.BytesIO(data))

        for utt_id, text in utterances.items():
            speaker, chapter, _ = utt_id.split("-")
            by_chapter.setdefault((speaker, chapter), []).append((utt_id, text))
            add(
                f"LibriSpeech/{subset}/{speaker}/{chapter}/{utt_id}.flac",
                b"flac:" + utt_id.encode(),
            )
        for (speaker, chapter), rows in by_chapter.items():
            body = "".join(f"{utt} {text}\n" for utt, text in rows).encode()
            add(f"LibriSpeech/{subset}/{speaker}/{chapter}/{speaker}-{chapter}.trans.txt", body)
    return path


@pytest.fixture
def stub_decode(monkeypatch):
    monkeypatch.setattr(_corpus, "_decode_flac", lambda data: tone(0.25))


@pytest.fixture
def tarball(tmp_path):
    return make_tarball(
        tmp_path / "dev-clean.tar.gz",
        "dev-clean",
        {
            "84-121123-0000": "HELLO WORLD",
            "84-121123-0001": "SECOND UTTERANCE",
            "174-50561-0000": "ANOTHER SPEAKER",
            "251-136532-0000": "THIRD SPEAKER",
        },
    )


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_writes_a_manifest_the_harness_can_load(tmp_path, tarball):
    out = tmp_path / "corpus"
    manifest_path = _build_corpus(out, tarball, n=3, subset="dev-clean")

    clips = load_manifest(manifest_path)
    assert len(clips) == 3
    assert all(clip.path.exists() for clip in clips)
    assert all(clip.sample_rate_hz == RATE and clip.channels == 1 for clip in clips)


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_spreads_the_selection_across_speakers(tmp_path, tarball):
    manifest_path = _build_corpus(tmp_path / "corpus", tarball, n=3, subset="dev-clean")
    speakers = {clip.source.split(":")[-1].split("-")[0] for clip in load_manifest(manifest_path)}
    assert speakers == {"84", "174", "251"}


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_carries_the_reference_transcript_and_licence(tmp_path, tarball):
    manifest_path = _build_corpus(tmp_path / "corpus", tarball, n=1, subset="dev-clean")
    (clip,) = load_manifest(manifest_path)
    assert clip.text == "HELLO WORLD"
    assert clip.license == "CC-BY-4.0"
    assert clip.source.startswith("librispeech:dev-clean:")


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_writes_the_attribution_notice(tmp_path, tarball):
    out = tmp_path / "corpus"
    _build_corpus(out, tarball, n=1, subset="dev-clean")
    notice = (out / "NOTICE").read_text(encoding="utf-8")
    assert "CC-BY-4.0" in notice and "Panayotov" in notice


@pytest.mark.usefixtures("stub_decode")
def test_other_subsets_are_categorised_as_accent(tmp_path):
    tar = make_tarball(tmp_path / "dev-other.tar.gz", "dev-other", {"84-121123-0000": "HARD ONE"})
    manifest_path = _build_corpus(tmp_path / "corpus", tar, n=1, subset="dev-other")
    assert load_manifest(manifest_path)[0].category == "accent"


@pytest.mark.usefixtures("stub_decode")
def test_clean_subsets_are_categorised_as_quiet(tmp_path, tarball):
    manifest_path = _build_corpus(tmp_path / "corpus", tarball, n=1, subset="dev-clean")
    assert load_manifest(manifest_path)[0].category == "quiet"


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_asking_for_more_clips_than_exist_takes_all_of_them(tmp_path, tarball):
    manifest_path = _build_corpus(tmp_path / "corpus", tarball, n=99, subset="dev-clean")
    assert len(load_manifest(manifest_path)) == 4


@pytest.mark.usefixtures("stub_decode")
def test_build_corpus_ignores_utterances_with_no_transcript(tmp_path):
    tar = tmp_path / "dev-clean.tar.gz"
    with tarfile.open(tar, "w:gz") as tf:
        info = tarfile.TarInfo("LibriSpeech/dev-clean/84/121123/84-121123-0000.flac")
        info.size = 4
        tf.addfile(info, io.BytesIO(b"flac"))
    manifest_path = _build_corpus(tmp_path / "corpus", tar, n=5, subset="dev-clean")
    assert load_manifest(manifest_path) == ()


# ─── cmd_download ────────────────────────────────────────────────────────────


class DownloadArgs:
    def __init__(self, out, cache, n=2, subset="dev-clean"):
        self.out = str(out)
        self.cache = str(cache)
        self.n = n
        self.subset = subset


def test_download_refuses_before_fetching_anything_when_ffmpeg_is_missing(tmp_path, monkeypatch):
    fetched = []
    monkeypatch.setattr(
        _corpus.subprocess,
        "run",
        lambda cmd, **kw: (_ for _ in ()).throw(FileNotFoundError("ffmpeg")),
    )
    monkeypatch.setattr(_corpus, "_download", lambda url, dest: fetched.append(url))

    with pytest.raises(SystemExit, match="ffmpeg is required"):
        cmd_download(DownloadArgs(tmp_path / "corpus", tmp_path / "cache"))
    assert fetched == []


@pytest.mark.usefixtures("stub_decode")
def test_download_builds_the_corpus_from_the_fetched_tarball(
    tmp_path, tarball, monkeypatch, capsys
):
    monkeypatch.setattr(
        _corpus.subprocess, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0)
    )
    monkeypatch.setattr(_corpus, "_download", lambda url, dest: tarball)

    out = tmp_path / "corpus"
    cmd_download(DownloadArgs(out, tmp_path / "cache", n=2))

    assert len(load_manifest(out / "manifest.json")) == 2
    assert "manifest:" in capsys.readouterr().out


def test_download_reuses_a_cached_tarball_without_refetching(tmp_path, monkeypatch, capsys):
    cached = tmp_path / "dev-clean.tar.gz"
    cached.write_bytes(b"already here")
    monkeypatch.setattr(
        _corpus.urllib.request, "urlopen", lambda *a, **k: pytest.fail("refetched a cached tarball")
    )
    assert _corpus._download("https://example.invalid/x.tar.gz", cached) == cached
    assert "using cached" in capsys.readouterr().out


# ─── cmd_make ────────────────────────────────────────────────────────────────


class MakeArgs:
    def __init__(self, dir, out=None, language="en", category="quiet"):  # noqa: A002
        self.dir = str(dir)
        self.out = str(out) if out else None
        self.language = language
        self.category = category


def test_make_builds_a_loadable_manifest_from_wavs_and_txt_sidecars(tmp_path):
    write_wav(tmp_path / "one.wav")
    (tmp_path / "one.txt").write_text("hello world\n", encoding="utf-8")

    cmd_make(MakeArgs(tmp_path))

    (clip,) = load_manifest(tmp_path / "manifest.json")
    assert clip.id == "one"
    assert clip.text == "hello world"
    assert clip.language == "en"
    assert clip.category == "quiet"
    assert clip.license == "unknown"
    assert clip.source == "user-provided:one.wav"


def test_make_honours_a_per_clip_category_sidecar(tmp_path):
    write_wav(tmp_path / "one.wav")
    (tmp_path / "one.txt").write_text("hello", encoding="utf-8")
    (tmp_path / "one.category").write_text("noise\n", encoding="utf-8")

    cmd_make(MakeArgs(tmp_path, category="quiet"))

    assert load_manifest(tmp_path / "manifest.json")[0].category == "noise"


def test_make_writes_the_manifest_to_a_separate_out_dir_with_relative_paths(tmp_path):
    src = tmp_path / "clips"
    src.mkdir()
    write_wav(src / "one.wav")
    (src / "one.txt").write_text("hello", encoding="utf-8")
    out = tmp_path / "out"

    cmd_make(MakeArgs(src, out=out))

    manifest = json.loads((out / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["clips"][0]["path"] == "one.wav"


def test_make_skips_clips_with_no_transcript_sidecar(tmp_path, capsys):
    write_wav(tmp_path / "kept.wav")
    (tmp_path / "kept.txt").write_text("hello", encoding="utf-8")
    write_wav(tmp_path / "orphan.wav")

    cmd_make(MakeArgs(tmp_path))

    assert [c.id for c in load_manifest(tmp_path / "manifest.json")] == ["kept"]
    assert "orphan.wav" in capsys.readouterr().out


def test_make_skips_clips_whose_sidecar_is_blank(tmp_path):
    write_wav(tmp_path / "kept.wav")
    (tmp_path / "kept.txt").write_text("hello", encoding="utf-8")
    write_wav(tmp_path / "blank.wav")
    (tmp_path / "blank.txt").write_text("   \n", encoding="utf-8")

    cmd_make(MakeArgs(tmp_path))

    assert [c.id for c in load_manifest(tmp_path / "manifest.json")] == ["kept"]


def test_make_skips_an_unreadable_wav_instead_of_aborting_the_run(tmp_path, capsys):
    write_wav(tmp_path / "kept.wav")
    (tmp_path / "kept.txt").write_text("hello", encoding="utf-8")
    (tmp_path / "broken.wav").write_bytes(b"not a RIFF header")
    (tmp_path / "broken.txt").write_text("hello", encoding="utf-8")

    cmd_make(MakeArgs(tmp_path))

    assert [c.id for c in load_manifest(tmp_path / "manifest.json")] == ["kept"]
    assert "skipping broken.wav" in capsys.readouterr().out


def test_make_on_a_missing_directory_exits(tmp_path):
    with pytest.raises(SystemExit, match="not a directory"):
        cmd_make(MakeArgs(tmp_path / "absent"))


def test_make_with_no_wavs_exits(tmp_path):
    with pytest.raises(SystemExit, match="no \\*.wav files found"):
        cmd_make(MakeArgs(tmp_path))


def test_make_with_no_usable_clips_explains_the_sidecar_convention(tmp_path):
    write_wav(tmp_path / "orphan.wav")
    with pytest.raises(SystemExit, match="matching .txt sidecar"):
        cmd_make(MakeArgs(tmp_path))
