"""The Mozilla to FLEURS language table covers every language a plugin names."""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest

from myna.benchmarker import fleurs_codes as fc

REPO = Path(__file__).resolve().parents[3]


def plugin_languages() -> set[str]:
    langs: set[str] = set()
    for path in REPO.glob("*-snap/plugins/*.toml"):
        plugin = tomllib.loads(path.read_text(encoding="utf-8"))
        if plugin["task"] == "text.translate":
            langs |= {plugin["language"], plugin["to"]}
    return langs


@pytest.mark.repo_tree
def test_every_plugin_language_maps_or_is_explicitly_no_eval():
    langs = plugin_languages()
    assert langs, "no translation plugin found: the repo tree is not here"
    for lang in langs:
        assert fc.evaluable(lang) or lang in fc.NO_EVAL, f"{lang} is in neither table"


def test_the_tables_do_not_overlap_and_codes_are_unique_fleurs_locales():
    assert not set(fc.MOZILLA_TO_FLEURS) & fc.NO_EVAL
    codes = list(fc.MOZILLA_TO_FLEURS.values())
    assert len(codes) == len(set(codes))
    assert all("_" in code for code in codes)


@pytest.mark.parametrize(
    ("lang", "code"),
    [
        ("zh-Hans", "cmn_hans_cn"),
        ("nb", "nb_no"),
        ("tl", "fil_ph"),
        ("fa", "fa_ir"),
        ("fr", "fr_fr"),
    ],
)
def test_the_documented_irregular_codes(lang, code):
    assert fc.fleurs_code(lang) == code


def test_no_eval_and_unknown_languages_are_told_apart():
    with pytest.raises(fc.NoEvalError, match="no-eval"):
        fc.fleurs_code("nn")
    with pytest.raises(fc.NoEvalError, match="not in the Mozilla"):
        fc.fleurs_code("xx")
    assert not fc.evaluable("zh-Hant")
