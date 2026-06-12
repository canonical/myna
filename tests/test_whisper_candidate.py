"""Candidate metadata for the faster-whisper adapter (no model/extra needed).

Constructing the adapter and reading ``.candidate`` does not import
faster_whisper, so this runs in the default offline suite. It guards the
label normalisation that keeps result records readable when the snap loads
weights from a local CTranslate2 model-component directory (T15).
"""

from myna.testbed.whisper import FasterWhisperAdapter, _iso639_1


def test_iso639_1_drops_region_subtag():
    # faster-whisper rejects BCP-47 region tags; the corpus uses them.
    assert _iso639_1("en-GB") == "en"
    assert _iso639_1("de") == "de"
    assert _iso639_1(None) is None  # auto-detect


def test_candidate_labels_a_bare_size():
    cand = FasterWhisperAdapter("small").candidate
    assert cand.model == "whisper-small"
    assert cand.engine == "faster-whisper-cpu"
    assert cand.streaming_strategy == "commit-on-finalize"


def test_candidate_labels_a_component_directory_by_leaf():
    # snap passes --model $SNAP_COMPONENTS/model-small
    cand = FasterWhisperAdapter(
        "/snap/whisper/components/42/model-small/", device="cuda"
    ).candidate
    assert cand.model == "whisper-model-small"  # leaf, not the absolute path
    assert cand.engine == "faster-whisper-cuda"
