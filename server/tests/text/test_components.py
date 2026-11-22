"""Discovery of model components."""

from __future__ import annotations

import logging
from pathlib import Path

from conftest import TAGGER_META, TRANSLATE_META, write_component

from myna.text.components import find_models
from myna.text.tagger import TaggerModel
from myna.text.translate import TranslateModel

BINARY = Path("/snap/myna-text/current/bin/myna-bergamot")


def test_each_component_with_a_manifest_is_a_model(tmp_path):
    write_component(tmp_path, "model-disfluency-en", TAGGER_META, ("tagger.onnx", "tokenizer.json"))
    write_component(tmp_path, "model-translate-en-fr", TRANSLATE_META, ("config.yml",))

    models = find_models(tmp_path, BINARY)

    assert [(type(m), m.id) for m in models] == [
        (TaggerModel, "myna-disfluency-en"),
        (TranslateModel, "bergamot-base-memory-en-fr"),
    ]


def test_a_missing_root_and_directories_without_a_manifest_are_no_models(tmp_path):
    (tmp_path / "gpu-nvidia").mkdir()
    (tmp_path / "stray-file").write_text("x")

    assert find_models(tmp_path, BINARY) == []
    assert find_models(tmp_path / "absent", BINARY) == []


def test_a_component_that_cannot_be_used_is_skipped_and_named_in_the_log(tmp_path, caplog):
    write_component(tmp_path, "a-nofiles", TAGGER_META, ("tagger.onnx",))
    write_component(tmp_path, "b-nokey", {"kind": "translate"}, ())
    write_component(tmp_path, "c-kind", {"kind": "mystery"}, ())
    write_component(tmp_path, "d-notjson", {}, ())
    (tmp_path / "d-notjson" / "myna-text-model.json").write_text("{")
    write_component(tmp_path, "e-list", {}, ())
    (tmp_path / "e-list" / "myna-text-model.json").write_text("[]")
    write_component(tmp_path, "f-good", TRANSLATE_META, ("config.yml",))

    with caplog.at_level(logging.WARNING, logger="myna.text"):
        models = find_models(tmp_path, BINARY)

    assert [m.id for m in models] == ["bergamot-base-memory-en-fr"]
    for name in ("a-nofiles", "b-nokey", "c-kind", "d-notjson", "e-list"):
        assert name in caplog.text
    assert "mystery" in caplog.text
