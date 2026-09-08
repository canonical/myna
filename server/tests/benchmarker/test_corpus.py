"""``myna-bench make-corpus``: a manifest from a tester's own recordings.

The output has to load through `myna.testbed.corpus.load_manifest` exactly like
a downloaded tier, so the tests assert against that loader. The rest is about
what happens to a clip that is not usable: skipping it and saying so beats
aborting a run someone has already recorded audio for.
"""

from __future__ import annotations

import array
import json
import wave

import pytest

from myna.benchmarker._audio import RATE
from myna.benchmarker._corpus import cmd_make
from myna.testbed.corpus import load_manifest


def tone(seconds: float = 0.25, rate: int = RATE) -> array.array:
    return array.array("h", [(i % 400) - 200 for i in range(int(seconds * rate))])


def write_wav(path, seconds: float = 0.25, rate: int = RATE, channels: int = 1) -> None:
    with wave.open(str(path), "w") as wf:
        wf.setnchannels(channels)
        wf.setsampwidth(2)
        wf.setframerate(rate)
        wf.writeframes(tone(seconds, rate).tobytes() * channels)


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
    assert manifest["clips"][0]["path"] == "../clips/one.wav"
    assert [c.id for c in load_manifest(out / "manifest.json")] == ["one"]


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
