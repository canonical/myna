"""The full-split presets behind ``myna-bench download-corpus --preset``.

Each preset's parser runs against a tiny fake archive built in tmp_path, and
the manifest it writes is read back through `load_manifest` and
`verify_corpus`: a publication corpus the harness cannot load, or whose id
does not survive a re-hash, is the failure worth catching. ffmpeg is the one
seam stubbed out, and the network is never touched.
"""

from __future__ import annotations

import array
import io
import json
import tarfile
from pathlib import Path

import pytest

from myna.benchmarker import corpus_english, corpus_publication
from myna.benchmarker._audio import RATE
from myna.benchmarker.corpus_publication import (
    FLEURS_REVISION,
    Preset,
    build_fleurs,
    build_fleurs_smoke,
    build_librispeech,
    cmd_preset,
    default_cache,
    parse_preset,
)
from myna.testbed.corpus import load_manifest, sha256_file, verify_corpus


def tone(seconds: float = 0.25) -> array.array:
    return array.array("h", [(i % 400) - 200 for i in range(int(seconds * RATE))])


@pytest.fixture(autouse=True)
def stub_decode(monkeypatch):
    """Every "archive member" decodes to a tone whose length encodes its size,
    so a clip's duration says which member it came from."""
    monkeypatch.setattr(corpus_english, "decode_audio", lambda data: tone(len(data) / 100))


def add(tar: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    tar.addfile(info, io.BytesIO(data))


# ─── parse_preset ────────────────────────────────────────────────────────────


def test_librispeech_presets_name_the_full_test_splits():
    assert parse_preset("librispeech-test-clean") == Preset(
        "librispeech-test-clean", "librispeech", "test-clean", None
    )
    assert parse_preset("librispeech-test-other").split == "test-other"


def test_a_fleurs_preset_carries_its_locale():
    assert parse_preset("fleurs-test:fr_fr") == Preset(
        "fleurs-test:fr_fr", "fleurs", "test", "fr_fr"
    )


def test_a_preset_slug_is_safe_as_a_directory_name():
    assert parse_preset("fleurs-test:cmn_hans_cn").slug == "fleurs-test-cmn_hans_cn"


@pytest.mark.parametrize(
    "name",
    ["librispeech-dev-clean", "fleurs-test", "fleurs-test:", "fleurs-test:../x", "ami-test"],
)
def test_an_unknown_preset_is_refused_with_the_list(name):
    with pytest.raises(SystemExit, match="librispeech-test-clean"):
        parse_preset(name)


def test_the_cache_is_the_shared_myna_one(monkeypatch, tmp_path):
    monkeypatch.delenv("XDG_CACHE_HOME", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))
    assert default_cache() == tmp_path / ".cache" / "myna" / "corpus-src"


def test_the_cache_follows_xdg_cache_home(monkeypatch, tmp_path):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "xdg"))
    assert default_cache() == tmp_path / "xdg" / "myna" / "corpus-src"


# ─── LibriSpeech: the whole split ────────────────────────────────────────────


def librispeech_tar(path: Path, split: str, utterances: dict[str, str], *, orphan=None) -> Path:
    """Transcripts deliberately *after* their audio, as in the real archive."""
    with tarfile.open(path, "w:gz") as tar:
        chapters: dict[str, list[str]] = {}
        for utt, text in utterances.items():
            speaker, chapter, _ = utt.split("-")
            add(tar, f"LibriSpeech/{split}/{speaker}/{chapter}/{utt}.flac", b"f" * 50)
            chapters.setdefault(f"{speaker}-{chapter}", []).append(f"{utt} {text}\n")
        if orphan:
            speaker, chapter, _ = orphan.split("-")
            add(tar, f"LibriSpeech/{split}/{speaker}/{chapter}/{orphan}.flac", b"f" * 50)
        for key, lines in chapters.items():
            speaker, chapter = key.split("-")
            body = "".join(lines).encode()
            add(tar, f"LibriSpeech/{split}/{speaker}/{chapter}/{key}.trans.txt", body)
        add(tar, "LibriSpeech/README.TXT", b"readme")
        add(tar, f"LibriSpeech/{split}/SPEAKERS.TXT", b"not audio, not a transcript")
    return path


UTTERANCES = {
    "1089-134686-0000": "HE HOPED THERE WOULD BE STEW",
    "1089-134686-0001": "STUFF IT INTO YOU",
    "121-121726-0000": "ALSO A POPULAR CONTRIVANCE",
}


def test_librispeech_takes_every_utterance_with_its_transcript(tmp_path):
    tar = librispeech_tar(tmp_path / "test-clean.tar.gz", "test-clean", UTTERANCES)
    manifest = build_librispeech(tmp_path / "out", tar, "test-clean")

    clips = {c.id: c for c in load_manifest(manifest)}
    assert set(clips) == {f"librispeech-{u}" for u in UTTERANCES}
    clip = clips["librispeech-121-121726-0000"]
    assert clip.text == "ALSO A POPULAR CONTRIVANCE"
    assert (clip.language, clip.category, clip.license) == ("en", "quiet", "CC-BY-4.0")
    assert clip.source == "librispeech:test-clean:121-121726-0000"
    assert clip.duration_seconds == pytest.approx(0.5)


def test_a_whole_split_reports_progress_as_it_decodes(tmp_path, monkeypatch, capsys):
    monkeypatch.setattr(corpus_publication, "PROGRESS_EVERY", 2)
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES)
    build_librispeech(tmp_path / "ls", tar, "test-clean")
    tsv, fleurs_tar = fleurs_files(tmp_path)
    build_fleurs(tmp_path / "fl", tsv, fleurs_tar, "fr_fr")
    out = capsys.readouterr().out
    assert "2 utterances decoded" in out and "2 clips decoded" in out


def test_librispeech_adds_no_noise_or_long_form_variants(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES)
    clips = load_manifest(build_librispeech(tmp_path / "out", tar, "test-clean"))
    assert {c.category for c in clips} == {"quiet"}


def test_librispeech_other_is_the_accent_category(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-other", UTTERANCES)
    clips = load_manifest(build_librispeech(tmp_path / "out", tar, "test-other"))
    assert {c.category for c in clips} == {"accent"}


def test_librispeech_drops_audio_that_has_no_transcript(tmp_path, capsys):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES, orphan="7-7-0000")
    manifest = build_librispeech(tmp_path / "out", tar, "test-clean")

    assert "librispeech-7-7-0000" not in {c.id for c in load_manifest(manifest)}
    assert not (tmp_path / "out" / "audio" / "librispeech-7-7-0000.wav").exists()
    assert "1 utterance(s) without a transcript" in capsys.readouterr().out


def test_librispeech_ignores_another_split_in_the_same_archive(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-other", UTTERANCES)
    with pytest.raises(SystemExit, match="no utterances"):
        build_librispeech(tmp_path / "out", tar, "test-clean")


def test_librispeech_manifest_names_licence_source_split_and_corpus_id(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES)
    manifest_path = build_librispeech(tmp_path / "out", tar, "test-clean")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    assert manifest["preset"] == "librispeech-test-clean"
    assert manifest["dataset"] == "librispeech"
    assert manifest["split"] == "test-clean"
    assert manifest["license"] == "CC-BY-4.0"
    assert manifest["source"] == "https://www.openslr.org/resources/12/test-clean.tar.gz"
    # The URL says where the archive came from; only its hash says which bytes.
    assert manifest["source_sha256"] == {"test-clean.tar.gz": sha256_file(tar)}
    assert manifest["corpus_id"] == verify_corpus(manifest_path)


def test_librispeech_writes_the_attribution_notice(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES)
    build_librispeech(tmp_path / "out", tar, "test-clean")
    notice = (tmp_path / "out" / "NOTICE").read_text(encoding="utf-8")
    assert "Panayotov" in notice and "CC-BY-4.0" in notice and "test-clean" in notice


def test_a_rebuild_yields_the_same_corpus_id(tmp_path):
    tar = librispeech_tar(tmp_path / "t.tar.gz", "test-clean", UTTERANCES)
    first = verify_corpus(build_librispeech(tmp_path / "a", tar, "test-clean"))
    assert verify_corpus(build_librispeech(tmp_path / "b", tar, "test-clean")) == first


# ─── FLEURS: one language's whole test split ─────────────────────────────────


FLEURS_ROWS = [
    # id, filename, raw, normalised, chars, num_samples, gender
    ("1829", "111.wav", 'Les "voyageurs" peuvent, parfois.', "les voyageurs peuvent parfois"),
    ("1829", "222.wav", 'Les "voyageurs" peuvent, parfois.', "les voyageurs peuvent parfois"),
    ("1830", "333.wav", "Un autre.", "un autre"),
]


def fleurs_files(tmp_path: Path, rows=FLEURS_ROWS, *, extra_wav=None) -> tuple[Path, Path]:
    tsv = tmp_path / "test.tsv"
    tsv.write_text(
        "".join(f"{i}\t{f}\t{raw}\t{norm}\tx |\t16000\tFEMALE\n" for i, f, raw, norm in rows),
        encoding="utf-8",
    )
    tar_path = tmp_path / "test.tar.gz"
    with tarfile.open(tar_path, "w:gz") as tar:
        tar.addfile(tarfile.TarInfo("test"))  # the directory entry
        for _, filename, _, _ in rows:
            add(tar, f"test/{filename}", b"w" * 25)
        if extra_wav:
            add(tar, f"test/{extra_wav}", b"w" * 25)
    return tsv, tar_path


def test_fleurs_takes_every_listed_clip_with_its_raw_transcription(tmp_path):
    tsv, tar = fleurs_files(tmp_path)
    clips = {c.id: c for c in load_manifest(build_fleurs(tmp_path / "out", tsv, tar, "fr_fr"))}

    assert set(clips) == {"fleurs-fr_fr-111", "fleurs-fr_fr-222", "fleurs-fr_fr-333"}
    clip = clips["fleurs-fr_fr-111"]
    # Quotes survive: the TSV is not CSV-quoted, and a csv reader would eat them.
    assert clip.text == 'Les "voyageurs" peuvent, parfois.'
    assert (clip.language, clip.category, clip.license) == ("fr", "non-english", "CC-BY-4.0")
    assert clip.source == "fleurs:fr_fr:test:111.wav"
    assert clip.duration_seconds == pytest.approx(0.25)


def test_fleurs_english_is_scored_as_english(tmp_path):
    tsv, tar = fleurs_files(tmp_path)
    clips = load_manifest(build_fleurs(tmp_path / "out", tsv, tar, "en_us"))
    assert {(c.language, c.category) for c in clips} == {("en", "quiet")}


def test_fleurs_mandarin_is_tagged_zh_and_unspaced_at_cjk(tmp_path):
    rows = [("1", "1.wav", "邓迪大学 University of Dundee 的 教授", "x")]
    tsv, tar = fleurs_files(tmp_path, rows)
    (clip,) = load_manifest(build_fleurs(tmp_path / "out", tsv, tar, "cmn_hans_cn"))
    assert clip.language == "zh"
    assert clip.text == "邓迪大学University of Dundee的教授"


def test_fleurs_skips_audio_the_tsv_does_not_list(tmp_path):
    tsv, tar = fleurs_files(tmp_path, extra_wav="999.wav")
    clips = load_manifest(build_fleurs(tmp_path / "out", tsv, tar, "fr_fr"))
    assert "fleurs-fr_fr-999" not in {c.id for c in clips}


def test_fleurs_ignores_short_rows_and_rows_with_no_transcription(tmp_path):
    tsv, tar = fleurs_files(tmp_path)
    text = tsv.read_text(encoding="utf-8").splitlines(keepends=True)
    text[0] = "1829\t111.wav\t \tx\tx |\t1\tMALE\n"
    tsv.write_text("".join(text) + "truncated\trow\n", encoding="utf-8")
    clips = load_manifest(build_fleurs(tmp_path / "out", tsv, tar, "fr_fr"))
    assert {c.id for c in clips} == {"fleurs-fr_fr-222", "fleurs-fr_fr-333"}


def test_fleurs_says_how_many_listed_clips_had_no_audio(tmp_path, capsys):
    tsv, tar = fleurs_files(tmp_path)
    with tsv.open("a", encoding="utf-8") as fp:
        fp.write("9\tmissing.wav\tGone.\tgone\tx |\t1\tMALE\n")
    build_fleurs(tmp_path / "out", tsv, tar, "fr_fr")
    assert "1 listed clip(s) had no audio" in capsys.readouterr().out


def test_fleurs_with_nothing_matching_is_an_error(tmp_path):
    tsv, tar = fleurs_files(tmp_path)
    tsv.write_text("", encoding="utf-8")
    with pytest.raises(SystemExit, match="no clips"):
        build_fleurs(tmp_path / "out", tsv, tar, "fr_fr")


def test_fleurs_manifest_names_licence_pinned_source_split_and_corpus_id(tmp_path):
    tsv, tar = fleurs_files(tmp_path)
    manifest_path = build_fleurs(tmp_path / "out", tsv, tar, "fr_fr")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))

    assert manifest["preset"] == "fleurs-test:fr_fr"
    assert (manifest["dataset"], manifest["split"], manifest["language"]) == (
        "fleurs",
        "test",
        "fr_fr",
    )
    assert manifest["license"] == "CC-BY-4.0"
    assert FLEURS_REVISION in manifest["source"]
    assert manifest["source_sha256"] == {
        "test.tsv": sha256_file(tsv),
        "audio/test.tar.gz": sha256_file(tar),
    }
    assert manifest["reference"] == "raw_transcription"
    assert manifest["corpus_id"] == verify_corpus(manifest_path)
    notice = (tmp_path / "out" / "NOTICE").read_text(encoding="utf-8")
    assert "Conneau" in notice and "fr_fr" in notice


# ─── cmd_preset ──────────────────────────────────────────────────────────────


class Args:
    def __init__(self, preset, cache, out=None, **legacy):
        self.preset = preset
        self.cache = str(cache)
        self.out = str(out) if out else None
        self.manifest_name = "manifest.json"
        self.subset = legacy.get("subset")
        self.n = legacy.get("n")
        self.select = legacy.get("select")
        self.tarball = legacy.get("tarball")
        self.long_form_minutes = legacy.get("long_form_minutes")
        self.skip_complete = legacy.get("skip_complete", False)


@pytest.fixture
def fetched(monkeypatch):
    """Record every URL asked for; hand back what the test staged in the cache."""
    urls: list[tuple[str, Path]] = []

    def download(url, dest):
        urls.append((url, dest))
        assert dest.exists(), f"test did not stage {dest}"
        return dest

    monkeypatch.setattr(corpus_english, "download", download)
    monkeypatch.setattr(corpus_english, "require_ffmpeg", lambda: None)
    return urls


def test_cmd_preset_builds_librispeech_from_the_shared_cache(tmp_path, fetched, monkeypatch):
    cache = tmp_path / "cache"
    (cache / "librispeech").mkdir(parents=True)
    librispeech_tar(cache / "librispeech" / "test-clean.tar.gz", "test-clean", UTTERANCES)
    monkeypatch.chdir(tmp_path)

    cmd_preset(Args("librispeech-test-clean", cache))

    assert fetched == [
        (
            "https://www.openslr.org/resources/12/test-clean.tar.gz",
            cache / "librispeech" / "test-clean.tar.gz",
        )
    ]
    assert (
        len(
            load_manifest(
                tmp_path / "corpus" / "publication" / "librispeech-test-clean" / "manifest.json"
            )
        )
        == 3
    )


def test_cmd_preset_fetches_both_fleurs_files_at_the_pinned_revision(tmp_path, fetched):
    cache = tmp_path / "cache"
    (cache / "fleurs" / "fr_fr").mkdir(parents=True)
    fleurs_files(cache / "fleurs" / "fr_fr")
    out = tmp_path / "fr"

    cmd_preset(Args("fleurs-test:fr_fr", cache, out))

    base = f"https://huggingface.co/datasets/google/fleurs/resolve/{FLEURS_REVISION}/data/fr_fr"
    assert fetched == [
        (f"{base}/test.tsv", cache / "fleurs" / "fr_fr" / "test.tsv"),
        (f"{base}/audio/test.tar.gz", cache / "fleurs" / "fr_fr" / "test.tar.gz"),
    ]
    assert len(load_manifest(out / "manifest.json")) == 3


def test_cmd_preset_reuses_a_built_corpus_without_decoding_again(
    tmp_path, fetched, monkeypatch, capsys
):
    cache = tmp_path / "cache"
    (cache / "librispeech").mkdir(parents=True)
    librispeech_tar(cache / "librispeech" / "test-clean.tar.gz", "test-clean", UTTERANCES)
    out = tmp_path / "out"
    cmd_preset(Args("librispeech-test-clean", cache, out))
    fetched.clear()
    monkeypatch.setattr(corpus_english, "decode_audio", lambda data: pytest.fail("decoded"))

    cmd_preset(Args("librispeech-test-clean", cache, out))

    assert fetched == []
    assert "already holds librispeech-test-clean" in capsys.readouterr().out


def test_cmd_preset_rebuilds_a_corpus_whose_audio_changed(tmp_path, fetched):
    cache = tmp_path / "cache"
    (cache / "librispeech").mkdir(parents=True)
    librispeech_tar(cache / "librispeech" / "test-clean.tar.gz", "test-clean", UTTERANCES)
    out = tmp_path / "out"
    cmd_preset(Args("librispeech-test-clean", cache, out))
    (out / "audio" / "librispeech-121-121726-0000.wav").write_bytes(b"tampered")
    fetched.clear()

    cmd_preset(Args("librispeech-test-clean", cache, out))

    assert len(fetched) == 1
    verify_corpus(out / "manifest.json")


def test_cmd_preset_refuses_to_overwrite_another_presets_corpus(tmp_path, fetched):
    cache = tmp_path / "cache"
    (cache / "librispeech").mkdir(parents=True)
    librispeech_tar(cache / "librispeech" / "test-clean.tar.gz", "test-clean", UTTERANCES)
    librispeech_tar(cache / "librispeech" / "test-other.tar.gz", "test-other", UTTERANCES)
    out = tmp_path / "out"
    cmd_preset(Args("librispeech-test-clean", cache, out))

    with pytest.raises(SystemExit, match="holds librispeech-test-clean"):
        cmd_preset(Args("librispeech-test-other", cache, out))


def test_cmd_preset_refuses_an_unreadable_manifest_rather_than_overwrite_it(tmp_path, fetched):
    out = tmp_path / "out"
    out.mkdir()
    (out / "manifest.json").write_text("{not json", encoding="utf-8")
    with pytest.raises(SystemExit, match="holds a corpus no preset built"):
        cmd_preset(Args("librispeech-test-clean", tmp_path, out))
    assert fetched == []


@pytest.mark.parametrize(
    "legacy",
    [
        {"subset": "dev-clean"},
        {"n": 5},
        {"n": 0},
        {"select": "balanced"},
        {"tarball": "x.tar.gz"},
        {"long_form_minutes": 5.0},
        {"skip_complete": True},
    ],
)
def test_cmd_preset_refuses_the_subset_selection_flags(tmp_path, fetched, legacy):
    with pytest.raises(SystemExit, match="--preset takes the whole split"):
        cmd_preset(Args("librispeech-test-clean", tmp_path, tmp_path / "out", **legacy))
    assert fetched == []


def test_cmd_preset_defaults_the_cache_to_the_shared_one(tmp_path, fetched, monkeypatch):
    monkeypatch.setenv("XDG_CACHE_HOME", str(tmp_path / "xdg"))
    cache = tmp_path / "xdg" / "myna" / "corpus-src"
    (cache / "librispeech").mkdir(parents=True)
    librispeech_tar(cache / "librispeech" / "test-other.tar.gz", "test-other", UTTERANCES)
    args = Args("librispeech-test-other", "unused", tmp_path / "out")
    args.cache = None

    cmd_preset(args)

    assert fetched[0][1] == cache / "librispeech" / "test-other.tar.gz"


def test_cmd_preset_checks_for_ffmpeg_before_any_download(tmp_path, fetched, monkeypatch):
    monkeypatch.setattr(
        corpus_english, "require_ffmpeg", lambda: (_ for _ in ()).throw(SystemExit("no ffmpeg"))
    )
    with pytest.raises(SystemExit, match="no ffmpeg"):
        cmd_preset(Args("fleurs-test:de_de", tmp_path, tmp_path / "out"))
    assert fetched == []


def test_the_module_names_every_preset_it_accepts():
    assert corpus_publication.PRESETS == (
        "librispeech-test-clean",
        "librispeech-test-other",
        "fleurs-test:<lang>",
        "fleurs-smoke",
    )


# ─── FLEURS smoke: a few clips of each language, one manifest ───────────────


def test_the_smoke_preset_is_parsed():
    assert parse_preset("fleurs-smoke").dataset == "fleurs-smoke"


def test_the_smoke_tier_takes_the_first_listed_clips_of_each_locale(tmp_path, monkeypatch):
    monkeypatch.setattr(corpus_publication, "SMOKE_CLIPS", 2)
    sources = {}
    for locale in ("fr_fr", "cmn_hans_cn"):
        (tmp_path / locale).mkdir()
        sources[locale] = fleurs_files(tmp_path / locale)
    clips = load_manifest(build_fleurs_smoke(tmp_path / "out", sources))
    assert [(c.id, c.language, c.category) for c in clips] == [
        ("fleurs-cmn_hans_cn-111", "zh", "cmn_hans_cn"),
        ("fleurs-cmn_hans_cn-222", "zh", "cmn_hans_cn"),
        ("fleurs-fr_fr-111", "fr", "fr_fr"),
        ("fleurs-fr_fr-222", "fr", "fr_fr"),
    ]


def test_the_smoke_tier_records_every_source_archive(tmp_path):
    (tmp_path / "fr_fr").mkdir()
    tsv, tar = fleurs_files(tmp_path / "fr_fr")
    manifest = json.loads(build_fleurs_smoke(tmp_path / "out", {"fr_fr": (tsv, tar)}).read_text())
    assert manifest["preset"] == "fleurs-smoke"
    assert manifest["source_sha256"] == {
        "fr_fr/test.tsv": sha256_file(tsv),
        "fr_fr/audio/test.tar.gz": sha256_file(tar),
    }
