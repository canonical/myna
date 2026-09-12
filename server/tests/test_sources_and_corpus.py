"""Tests for WavFileSource (T04) and the fixture corpus (T03)."""

import json
import math
import wave
from array import array
from pathlib import Path

import pytest

from myna.core import AudioFormat, LoopbackClient
from myna.testbed import FakeAdapter, Harness, WavFileSource, by_category, load_manifest
from myna.testbed.corpus import (
    SCHEMA_VERSION,
    corpus_id,
    sha256_file,
    stamp_corpus,
    verify_corpus,
)

FIXTURES = Path(__file__).parent.parent / "fixtures"


def write_tone_wav(path: Path, *, seconds=0.5, rate=16_000, channels=1, freq=440.0) -> None:
    n = int(seconds * rate)
    samples = array(
        "h",
        (int(10_000 * math.sin(2 * math.pi * freq * i / rate)) for i in range(n * channels)),
    )
    with wave.open(str(path), "wb") as wav:
        wav.setnchannels(channels)
        wav.setsampwidth(2)
        wav.setframerate(rate)
        wav.writeframes(samples.tobytes())


def make_manifest(tmp_path: Path, **overrides) -> Path:
    (tmp_path / "audio").mkdir()
    write_tone_wav(tmp_path / "audio" / "clip.wav")
    entry = {
        "id": "tone",
        "path": "audio/clip.wav",
        "text": "reference text",
        "language": "en",
        "category": "quiet",
        "duration_seconds": 0.5,
        "sample_rate_hz": 16_000,
        "channels": 1,
        "source": "synthetic:test",
        "license": "CC0-1.0",
        "sha256": sha256_file(tmp_path / "audio" / "clip.wav"),
        **overrides.pop("entry", {}),
    }
    manifest = {"schema_version": SCHEMA_VERSION, "clips": [entry], **overrides}
    path = tmp_path / "manifest.json"
    path.write_text(json.dumps(manifest))
    return path


async def test_wav_source_format_and_duration(tmp_path):
    write_tone_wav(tmp_path / "t.wav", seconds=0.5, rate=22_050, channels=2)
    source = WavFileSource(tmp_path / "t.wav", chunk_seconds=0.1)
    assert source.format == AudioFormat(sample_rate_hz=22_050, channels=2, sample_width_bytes=2)
    total = 0.0
    count = 0
    async for chunk in source.chunks():
        assert chunk.format == source.format
        total += chunk.duration_seconds
        count += 1
    assert total == pytest.approx(0.5, abs=0.01)
    assert count == 5


async def test_wav_source_chunk_size_configurable(tmp_path):
    write_tone_wav(tmp_path / "t.wav", seconds=0.3)
    chunks = [c async for c in WavFileSource(tmp_path / "t.wav", chunk_seconds=0.05).chunks()]
    assert len(chunks) == 6
    assert chunks[0].duration_seconds == pytest.approx(0.05)


def test_manifest_roundtrip_and_categories(tmp_path):
    clips = load_manifest(make_manifest(tmp_path))
    assert len(clips) == 1
    clip = clips[0]
    assert clip.path.is_absolute() and clip.path.exists()
    assert clip.text == "reference text"
    assert by_category(clips)["quiet"] == clips


def test_manifest_schema_version_checked(tmp_path):
    path = make_manifest(tmp_path, schema_version=999)
    with pytest.raises(ValueError, match="schema_version"):
        load_manifest(path)


async def test_harness_runs_clip_through_fake_adapter(tmp_path):
    clip = load_manifest(make_manifest(tmp_path))[0]
    adapter = FakeAdapter()
    record = await Harness().run(
        client=LoopbackClient(adapter),
        candidate=adapter.candidate,
        source=clip.open_source(),
    )
    assert record.audio_duration_seconds == pytest.approx(clip.duration_seconds, abs=0.01)
    assert record.transcript  # terminal done arrived


# --- checks against the generated corpus, skipped if not generated ---

generated = pytest.mark.skipif(
    not (FIXTURES / "manifest.json").exists(),
    reason="run `python dev/generate_fixtures.py` from repo root first",
)


@generated
def test_generated_corpus_covers_accuracy_matrix():
    categories = set(by_category(load_manifest(FIXTURES / "manifest.json")))
    assert categories >= {
        "quiet",
        "noise",
        "accent",
        "non-english",
        "commands",
        "long-form",
        "technical",
        "acronyms",
        "names",
    }


@generated
def test_generated_clips_exist_and_match_manifest():
    for clip in load_manifest(FIXTURES / "manifest.json"):
        source = clip.open_source()
        assert source.format.sample_rate_hz == clip.sample_rate_hz
        assert source.format.channels == clip.channels
        assert clip.license == "CC0-1.0"
        assert clip.text


def test_corpus_id_ignores_clip_order(tmp_path):
    """Identity is the set of clips, not how the manifest happens to list them."""
    path = make_manifest(tmp_path)
    manifest = json.loads(path.read_text())
    manifest["clips"].append(dict(manifest["clips"][0], id="tone-b"))
    path.write_text(json.dumps(manifest))
    forward = corpus_id(path)
    manifest["clips"].reverse()
    path.write_text(json.dumps(manifest))
    assert corpus_id(path) == forward


@pytest.mark.parametrize(
    "change",
    [
        pytest.param({"text": "different reference"}, id="reference-text"),
        pytest.param({"category": "noise"}, id="category"),
        pytest.param({"language": "de"}, id="language"),
        pytest.param({"sha256": "0" * 64}, id="audio-digest"),
    ],
)
def test_corpus_id_tracks_what_wer_is_scored_against(tmp_path, change):
    baseline = corpus_id(make_manifest(tmp_path))
    other = tmp_path / "other"
    other.mkdir()
    assert corpus_id(make_manifest(other, entry=change)) != baseline


def test_corpus_id_needs_every_clip_to_record_its_audio(tmp_path):
    path = make_manifest(tmp_path, entry={"sha256": ""})
    with pytest.raises(ValueError, match="records no sha256"):
        corpus_id(path)


def test_manifest_without_digests_does_not_load(tmp_path):
    path = make_manifest(tmp_path, entry={"sha256": ""})
    with pytest.raises(ValueError, match="records no sha256"):
        load_manifest(path)


def test_stamp_corpus_records_each_clip_and_the_set(tmp_path):
    path = make_manifest(tmp_path, entry={"sha256": ""})
    identity = stamp_corpus(path)
    manifest = json.loads(path.read_text())
    assert manifest["corpus_id"] == identity
    assert manifest["clips"][0]["sha256"] == sha256_file(tmp_path / "audio" / "clip.wav")
    assert stamp_corpus(path) == identity


def test_verify_corpus_names_the_clip_that_is_not_what_the_manifest_says(tmp_path):
    """Same path, different audio: the numbers a run recorded against the
    stamped id were not measured on what is on disk now."""
    path = make_manifest(tmp_path)
    stamp_corpus(path)
    write_tone_wav(tmp_path / "audio" / "clip.wav", seconds=0.4)
    with pytest.raises(ValueError, match="tone: "):
        verify_corpus(path)


def test_verify_corpus_reports_a_missing_clip(tmp_path):
    path = make_manifest(tmp_path)
    stamp_corpus(path)
    (tmp_path / "audio" / "clip.wav").unlink()
    with pytest.raises(ValueError, match="missing"):
        verify_corpus(path)


def test_stamp_corpus_will_not_relabel_drifted_audio(tmp_path):
    path = make_manifest(tmp_path)
    stamp_corpus(path)
    write_tone_wav(tmp_path / "audio" / "clip.wav", seconds=0.4)
    with pytest.raises(ValueError, match="not the recorded"):
        stamp_corpus(path)
