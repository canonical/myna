"""The LibriSpeech corpus builder behind ``myna-bench download-corpus``.

It produces the manifest schema `myna.testbed.corpus.load_manifest` consumes,
so the tests assert against that loader rather than against the JSON by eye: a
manifest this code writes and the harness cannot read is the failure worth
catching, and it is invisible to a field-by-field comparison.

No network and no ffmpeg: the LibriSpeech tarball is synthesised in tmp_path
and the FLAC decode is the one seam stubbed out.
"""

from __future__ import annotations

import array
import io
import subprocess
import tarfile

import pytest

from myna.benchmarker import corpus_english
from myna.benchmarker._audio import NOISE_SNR_DB, RATE
from myna.benchmarker.corpus_english import (
    N_NOISE,
    _round_robin,
    _speaker,
    build,
    cmd_download,
    decode_flac,
    download,
    is_complete,
    open_split,
)
from myna.testbed.corpus import load_manifest


def tone(seconds: float = 0.25, rate: int = RATE) -> array.array:
    return array.array("h", [(i % 400) - 200 for i in range(int(seconds * rate))])


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


# ─── decode_flac ────────────────────────────────────────────────────────────


def test_decode_flac_pipes_the_bytes_through_ffmpeg_at_16k_mono(monkeypatch):
    seen = {}

    def run(cmd, input=None, **kwargs):  # noqa: A002
        seen["cmd"] = cmd
        seen["input"] = input
        return subprocess.CompletedProcess(cmd, 0, stdout=tone(0.1).tobytes())

    monkeypatch.setattr(corpus_english.subprocess, "run", run)
    samples = decode_flac(b"fake-flac")

    assert seen["input"] == b"fake-flac"
    assert seen["cmd"][0] == "ffmpeg"
    assert "-ar" in seen["cmd"] and str(RATE) in seen["cmd"]
    assert seen["cmd"][seen["cmd"].index("-ac") + 1] == "1"
    assert len(samples) == int(0.1 * RATE)


# ─── build ───────────────────────────────────────────────────────────


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
    monkeypatch.setattr(corpus_english, "decode_flac", lambda data: tone(0.25))


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
def test_build_writes_a_manifest_the_harness_can_load(tmp_path, tarball):
    out = tmp_path / "corpus"
    manifest_path = build(out, tarball, 3, subset="dev-clean", select="balanced")

    clips = load_manifest(manifest_path)
    # 3 selected utterances plus the seeded-noise variants appended from the
    # first N_NOISE of them - the noise tier is part of every corpus, not an
    # option, so a count ignoring it would drift the moment N_NOISE moved.
    assert len(clips) == 3 + N_NOISE
    assert all(clip.path.exists() for clip in clips)
    assert all(clip.sample_rate_hz == RATE and clip.channels == 1 for clip in clips)


@pytest.mark.usefixtures("stub_decode")
def test_build_spreads_the_selection_across_speakers(tmp_path, tarball):
    manifest_path = build(tmp_path / "corpus", tarball, 3, subset="dev-clean", select="balanced")
    speakers = {clip.source.split(":")[-1].split("-")[0] for clip in load_manifest(manifest_path)}
    assert speakers == {"84", "174", "251"}


@pytest.mark.usefixtures("stub_decode")
def test_build_carries_the_reference_transcript_and_licence(tmp_path, tarball):
    manifest_path = build(tmp_path / "corpus", tarball, 1, subset="dev-clean", select="balanced")
    clip = load_manifest(manifest_path)[0]
    assert clip.text == "HELLO WORLD"
    assert clip.license == "CC-BY-4.0"
    assert clip.source.startswith("librispeech:dev-clean:")


@pytest.mark.usefixtures("stub_decode")
def test_build_writes_the_attribution_notice(tmp_path, tarball):
    out = tmp_path / "corpus"
    build(out, tarball, 1, subset="dev-clean", select="balanced")
    notice = (out / "NOTICE").read_text(encoding="utf-8")
    assert "CC-BY-4.0" in notice and "Panayotov" in notice


@pytest.mark.usefixtures("stub_decode")
def test_other_subsets_are_categorised_as_accent(tmp_path):
    tar = make_tarball(tmp_path / "dev-other.tar.gz", "dev-other", {"84-121123-0000": "HARD ONE"})
    manifest_path = build(tmp_path / "corpus", tar, 1, subset="dev-other", select="balanced")
    assert load_manifest(manifest_path)[0].category == "accent"


@pytest.mark.usefixtures("stub_decode")
def test_clean_subsets_are_categorised_as_quiet(tmp_path, tarball):
    manifest_path = build(tmp_path / "corpus", tarball, 1, subset="dev-clean", select="balanced")
    assert load_manifest(manifest_path)[0].category == "quiet"


@pytest.mark.usefixtures("stub_decode")
def test_build_asking_for_more_clips_than_exist_takes_all_of_them(tmp_path, tarball):
    manifest_path = build(tmp_path / "corpus", tarball, 99, subset="dev-clean", select="balanced")
    assert len(load_manifest(manifest_path)) == 4 + N_NOISE


@pytest.mark.usefixtures("stub_decode")
def test_build_refuses_a_tarball_whose_utterances_have_no_transcript(tmp_path):
    tar = tmp_path / "dev-clean.tar.gz"
    with tarfile.open(tar, "w:gz") as tf:
        info = tarfile.TarInfo("LibriSpeech/dev-clean/84/121123/84-121123-0000.flac")
        info.size = 4
        tf.addfile(info, io.BytesIO(b"flac"))
    with pytest.raises(SystemExit, match="no clips selected"):
        build(tmp_path / "corpus", tar, 5, subset="dev-clean", select="balanced")


# ─── cmd_download ────────────────────────────────────────────────────────────


class DownloadArgs:
    def __init__(self, out, cache, n=2, subset="dev-clean", **kw):
        self.out = str(out)
        self.cache = str(cache)
        self.n = n
        self.subset = subset
        self.tarball = kw.get("tarball")
        self.select = kw.get("select", "balanced")
        self.manifest_name = kw.get("manifest_name", "manifest.json")
        self.long_form_minutes = kw.get("long_form_minutes")
        self.skip_complete = kw.get("skip_complete", False)


def test_download_refuses_before_fetching_anything_when_ffmpeg_is_missing(tmp_path, monkeypatch):
    fetched = []
    monkeypatch.setattr(
        corpus_english.subprocess,
        "run",
        lambda cmd, **kw: (_ for _ in ()).throw(FileNotFoundError("ffmpeg")),
    )
    monkeypatch.setattr(corpus_english, "download", lambda url, dest: fetched.append(url))

    with pytest.raises(SystemExit, match="ffmpeg is required"):
        cmd_download(DownloadArgs(tmp_path / "corpus", tmp_path / "cache"))
    assert fetched == []


@pytest.mark.usefixtures("stub_decode")
def test_download_builds_the_corpus_from_the_fetched_tarball(
    tmp_path, tarball, monkeypatch, capsys
):
    monkeypatch.setattr(
        corpus_english.subprocess, "run", lambda cmd, **kw: subprocess.CompletedProcess(cmd, 0)
    )
    monkeypatch.setattr(corpus_english, "download", lambda url, dest: tarball)

    out = tmp_path / "corpus"
    cmd_download(DownloadArgs(out, tmp_path / "cache", n=2))

    assert len(load_manifest(out / "manifest.json")) == 2 + N_NOISE
    assert "manifest:" in capsys.readouterr().out


def test_build_appends_seeded_noise_variants_of_the_first_clips(tmp_path, tarball, stub_decode):
    manifest_path = build(tmp_path / "corpus", tarball, 3, subset="dev-clean", select="balanced")
    noisy = [c for c in load_manifest(manifest_path) if c.category == "noise"]
    assert len(noisy) == N_NOISE
    assert all(c.id.endswith(f"-noise-snr{int(NOISE_SNR_DB)}") for c in noisy)
    assert all(c.source.endswith("+noise") for c in noisy)


def test_archive_selection_takes_one_speaker_where_balanced_spreads(tmp_path, tarball, stub_decode):
    """The two strategies are not interchangeable, which is the whole point of
    keeping both: dev-clean's archive order lands every clip on one speaker."""
    archive = build(tmp_path / "a", tarball, 2, subset="dev-clean", select="archive")
    balanced = build(tmp_path / "b", tarball, 2, subset="dev-clean", select="balanced")

    def speakers(manifest):
        return {c.source.split(":")[-1].split("-")[0] for c in load_manifest(manifest)}

    assert speakers(archive) == {"84"}
    assert speakers(balanced) == {"84", "174"}


def test_is_complete_accepts_the_corpus_it_just_built(tmp_path, tarball, stub_decode):
    build(tmp_path / "corpus", tarball, 2, subset="dev-clean", select="balanced")
    assert is_complete(tmp_path / "corpus", "manifest.json", 2, "dev-clean")


def test_is_complete_rejects_a_different_split(tmp_path, tarball, stub_decode):
    build(tmp_path / "corpus", tarball, 2, subset="dev-clean", select="balanced")
    assert not is_complete(tmp_path / "corpus", "manifest.json", 2, "dev-other")


def test_is_complete_rejects_a_tier_missing_its_audio(tmp_path, tarball, stub_decode):
    out = tmp_path / "corpus"
    build(out, tarball, 2, subset="dev-clean", select="balanced")
    next(iter((out / "audio").glob("*.wav"))).unlink()
    assert not is_complete(out, "manifest.json", 2, "dev-clean")


def test_a_second_split_into_one_dir_is_refused(tmp_path, tarball, stub_decode):
    """One split per dir: the NOTICE carries that split's attribution, and a
    second one would silently overwrite it."""
    out = tmp_path / "corpus"
    build(out, tarball, 1, subset="dev-clean", select="balanced")
    other = make_tarball(tmp_path / "dev-other.tar.gz", "dev-other", {"84-121123-0000": "HARD"})
    with pytest.raises(SystemExit, match="already holds a different LibriSpeech split"):
        build(out, other, 1, subset="dev-other", select="balanced")


# ─── download: the cache must not be trusted blindly ─────────────────────────


class FakeResponse:
    def __init__(self, body: bytes, length=None):
        self._body = body
        self.headers = {"Content-Length": str(length if length is not None else len(body))}
        self._pos = 0

    def read(self, size=-1):
        chunk = self._body[self._pos :] if size < 0 else self._body[self._pos : self._pos + size]
        self._pos += len(chunk)
        return chunk

    def __enter__(self):
        return self

    def __exit__(self, *_):
        return False


def serve(monkeypatch, body: bytes, *, length=None, head_size="match"):
    """Stub urlopen for both the HEAD size probe and the GET."""
    calls = []

    def urlopen(request, *a, **kw):
        method = getattr(request, "method", "GET")
        calls.append(method)
        if method == "HEAD":
            if head_size == "unavailable":
                raise OSError("no HEAD")
            return FakeResponse(b"", length=length if length is not None else len(body))
        return FakeResponse(body, length=length)

    monkeypatch.setattr(corpus_english.urllib.request, "urlopen", urlopen)
    return calls


def test_a_complete_cache_is_reused_without_refetching(tmp_path, monkeypatch, capsys):
    dest = tmp_path / "dev-clean.tar.gz"
    dest.write_bytes(b"x" * 100)
    calls = serve(monkeypatch, b"x" * 100)

    assert download("https://example.invalid/x.tar.gz", dest) == dest

    assert calls == ["HEAD"]  # never fetched the body
    assert "using cached" in capsys.readouterr().out


def test_a_short_cache_is_refetched_rather_than_trusted(tmp_path, monkeypatch, capsys):
    """The real failure: a download killed at 7% left a 25 MB stub that every
    later run reported as "using cached" and then died inside tarfile."""
    dest = tmp_path / "dev-clean.tar.gz"
    dest.write_bytes(b"x" * 25)
    serve(monkeypatch, b"x" * 100)

    download("https://example.invalid/x.tar.gz", dest)

    assert dest.read_bytes() == b"x" * 100
    out = capsys.readouterr().out
    assert "expected" in out and "earlier download did not finish" in out


def test_an_unverifiable_cache_is_used_but_flagged(tmp_path, monkeypatch, capsys):
    dest = tmp_path / "dev-clean.tar.gz"
    dest.write_bytes(b"x" * 25)
    serve(monkeypatch, b"x" * 100, head_size="unavailable")

    download("https://example.invalid/x.tar.gz", dest)

    assert dest.read_bytes() == b"x" * 25  # offline: the cache is all there is
    assert "unverified" in capsys.readouterr().out


def test_a_download_lands_atomically(tmp_path, monkeypatch):
    dest = tmp_path / "dev-clean.tar.gz"
    serve(monkeypatch, b"x" * 100)
    download("https://example.invalid/x.tar.gz", dest)
    assert dest.read_bytes() == b"x" * 100
    assert not list(tmp_path.glob("*.part"))


def test_an_interrupted_download_leaves_no_cache_to_inherit(tmp_path, monkeypatch):
    """A .part that survived would be indistinguishable from a good cache on
    the next run, which is how the 25 MB stub came to exist."""
    dest = tmp_path / "dev-clean.tar.gz"

    class Interrupted(FakeResponse):
        def read(self, size=-1):
            raise KeyboardInterrupt

    def urlopen(request, *a, **kw):
        if getattr(request, "method", "GET") == "HEAD":
            return FakeResponse(b"", length=100)
        return Interrupted(b"")

    monkeypatch.setattr(corpus_english.urllib.request, "urlopen", urlopen)

    with pytest.raises(KeyboardInterrupt):
        download("https://example.invalid/x.tar.gz", dest)

    assert not dest.exists()
    assert not list(tmp_path.glob("*.part"))


def test_a_truncated_body_is_not_promoted_to_the_cache(tmp_path, monkeypatch):
    dest = tmp_path / "dev-clean.tar.gz"
    serve(monkeypatch, b"x" * 40, length=100)  # server promised 100, sent 40

    with pytest.raises(OSError, match="got 40 bytes, expected 100"):
        download("https://example.invalid/x.tar.gz", dest)

    assert not dest.exists()


# ─── open_split ──────────────────────────────────────────────────────────────


def test_a_truncated_archive_names_the_file_and_the_fix(tmp_path, tarball):
    short = tmp_path / "short.tar.gz"
    short.write_bytes(tarball.read_bytes()[: len(tarball.read_bytes()) // 3])

    with pytest.raises(SystemExit, match="not a complete LibriSpeech archive"):
        with open_split(short):
            pass


def test_a_file_that_is_not_an_archive_at_all_says_so(tmp_path):
    junk = tmp_path / "junk.tar.gz"
    junk.write_bytes(b"this is not a tarball")
    with pytest.raises(SystemExit, match="not a complete LibriSpeech archive"):
        with open_split(junk):
            pass


def test_a_truncation_found_mid_iteration_is_caught_too(tmp_path, tarball):
    """Where it actually bit: tarfile reads lazily, so the EOFError surfaces
    while walking members, not at open."""
    data = tarball.read_bytes()
    short = tmp_path / "short.tar.gz"
    short.write_bytes(data[: len(data) - 40])

    with pytest.raises(SystemExit, match="Delete it and re-run"):
        with open_split(short) as tar:
            list(tar)


def test_a_good_archive_reads_through_open_split(tarball):
    with open_split(tarball) as tar:
        assert any(m.name.endswith(".flac") for m in tar)
