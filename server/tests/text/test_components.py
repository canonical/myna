"""Discovery of model components."""

from __future__ import annotations

import logging
from pathlib import Path

from conftest import TAGGER_META, TRANSLATE_META, write_component

from myna.text.components import find_models
from myna.text.tagger import KINDS as TAGGER_KINDS
from myna.text.tagger import TaggerModel
from myna.translation.bergamot import TranslateModel, kinds

BINARY = Path("/snap/myna-translation/current/bin/myna-bergamot")
KINDS = {**TAGGER_KINDS, **kinds(BINARY)}


def test_each_component_with_a_manifest_is_a_model(tmp_path):
    write_component(tmp_path, "model-disfluency-en", TAGGER_META, ("tagger.onnx", "tokenizer.json"))
    write_component(tmp_path, "model-translate-en-fr", TRANSLATE_META, ("config.yml",))

    models = find_models(tmp_path, KINDS)

    assert [(type(m), m.id) for m in models] == [
        (TaggerModel, "myna-disfluency-en"),
        (TranslateModel, "bergamot-base-memory-en-fr"),
    ]


def test_a_missing_root_and_directories_without_a_manifest_are_no_models(tmp_path):
    (tmp_path / "gpu-nvidia").mkdir()
    (tmp_path / "stray-file").write_text("x")

    assert find_models(tmp_path, KINDS) == []
    assert find_models(tmp_path / "absent", KINDS) == []


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
        models = find_models(tmp_path, KINDS)

    assert [m.id for m in models] == ["bergamot-base-memory-en-fr"]
    for name in ("a-nofiles", "b-nokey", "c-kind", "d-notjson", "e-list"):
        assert name in caplog.text
    assert "mystery" in caplog.text


def test_a_directory_without_a_manifest_does_not_end_the_search(tmp_path):
    (tmp_path / "a-gpu-nvidia").mkdir()
    write_component(tmp_path, "b-translate", TRANSLATE_META, ("config.yml",))

    assert [m.id for m in find_models(tmp_path, KINDS)] == ["bergamot-base-memory-en-fr"]


def test_each_file_a_component_needs_is_checked(tmp_path):
    write_component(tmp_path, "a-onnx-only", TAGGER_META, ("tagger.onnx",))
    write_component(tmp_path, "b-tokenizer-only", TAGGER_META, ("tokenizer.json",))
    write_component(tmp_path, "c-no-config", TRANSLATE_META, ())

    assert find_models(tmp_path, KINDS) == []


def test_the_model_is_given_its_component_directory_and_the_translator_binary(tmp_path):
    write_component(tmp_path, "a-tagger", TAGGER_META, ("tagger.onnx", "tokenizer.json"))
    write_component(tmp_path, "b-translate", TRANSLATE_META, ("config.yml",))

    tagger, translate = find_models(tmp_path, KINDS)

    assert tagger._dir == tmp_path / "a-tagger"
    assert translate._worker._argv == [
        str(BINARY),
        "--model-config-paths",
        str(tmp_path / "b-translate" / "config.yml"),
    ]


def test_the_log_names_the_component_and_why(tmp_path, caplog):
    write_component(tmp_path, "c-kind", {"kind": "mystery"}, ())
    write_component(tmp_path, "d-nofile", TAGGER_META, ())
    write_component(tmp_path, "e-nokey", {"kind": "translate"}, ())
    (tmp_path / "f-bad").mkdir()
    (tmp_path / "f-bad" / "myna-text-model.json").write_text("{")

    with caplog.at_level(logging.WARNING, logger="myna.text"):
        find_models(tmp_path, KINDS)

    assert sorted(r.getMessage() for r in caplog.records) == [
        "c-kind: unknown kind 'mystery'",
        "d-nofile: not a usable model component (FileNotFoundError)",
        "e-nokey: not a usable model component (KeyError)",
        "f-bad: unreadable myna-text-model.json (JSONDecodeError)",
    ]


def test_a_snap_serves_only_the_kinds_it_registered(tmp_path, caplog):
    write_component(tmp_path, "a-tagger", TAGGER_META, ("tagger.onnx", "tokenizer.json"))
    write_component(tmp_path, "b-translate", TRANSLATE_META, ("config.yml",))

    with caplog.at_level(logging.WARNING, logger="myna.text"):
        assert [m.id for m in find_models(tmp_path, TAGGER_KINDS)] == ["myna-disfluency-en"]
        assert [m.id for m in find_models(tmp_path, kinds(BINARY))] == [
            "bergamot-base-memory-en-fr"
        ]

    assert sorted(r.getMessage() for r in caplog.records) == [
        "a-tagger: unknown kind 'disfluency-tagger'",
        "b-translate: unknown kind 'translate'",
    ]
