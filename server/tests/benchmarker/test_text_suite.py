"""`myna-bench text`: gates, pending, plug-in systems and the CLI exit status."""

from __future__ import annotations

import argparse
import json

import pytest

from myna.benchmarker import text_corpus, text_suite
from myna.benchmarker.text_suite import Gate, System, evaluate, reference_rule

ALL = sorted(text_suite.SUITES)


def gates(report):
    return {f"{g.suite}.{g.metric}": g for g in report.gates}


def test_identity_leaves_clean_text_alone_but_cleans_nothing():
    report = evaluate(text_suite.identity_system(), ALL, repeats=1)
    g = gates(report)
    assert g["clean_en.uer"].value == 0 and g["clean_en.uer"].status == "pass"
    assert g["disfluent_en.reduction"].value == 0
    assert g["disfluent_en.reduction"].status == "fail"
    assert g["disfluent_en.polarity_flips"].status == "pass"
    assert report.failed


def test_a_system_without_translation_leaves_those_gates_pending():
    report = evaluate(text_suite.identity_system(), ["translate_en_fr"], repeats=1)
    assert [g.status for g in report.gates] == ["pending", "pending"]
    assert report.pending and not report.failed


def test_a_system_without_tidy_leaves_cleanup_gates_pending_except_the_gold_check():
    report = evaluate(System("translator-only"), ["clean_en", "disfluent_en"], repeats=1)
    g = gates(report)
    assert g["clean_en.uer"].status == "pending"
    assert g["disfluent_en.reduction"].status == "pending"
    assert g["disfluent_en.polarity_flips"].status == "pending"
    assert g["disfluent_en.check_false_reject"].status == "pass"


def test_the_gold_edits_pass_the_conservative_edit_check():
    g = gates(evaluate(System("none"), ["disfluent_en"], repeats=1))
    assert g["disfluent_en.check_false_reject"].value == 0


def test_perfect_translation_passes_and_a_wrong_one_fails():
    rows = text_corpus.load().translate_en_fr
    lookup = {r.en: r.fr for r in rows}
    good = evaluate(System("oracle", translate=lookup.__getitem__), ["translate_en_fr"], 1)
    assert [g.status for g in good.gates] == ["pass", "pass"]
    assert gates(good)["translate_en_fr.chrf"].value == pytest.approx(1.0)
    bad = evaluate(System("echo", translate=lambda text: text), ["translate_en_fr"], 1)
    assert gates(bad)["translate_en_fr.chrf"].status == "fail"


def test_translation_p95_gate_fails_a_slow_system(monkeypatch):
    ticks = iter(range(0, 10**7, 1))
    monkeypatch.setattr(text_suite.time, "perf_counter", lambda: next(ticks) * 0.5)
    lookup = {r.en: r.fr for r in text_corpus.load().translate_en_fr}
    report = evaluate(System("slow", translate=lookup.__getitem__), ["translate_en_fr"], 1)
    p95 = gates(report)["translate_en_fr.p95_ms"]
    assert p95.value == 500.0 and p95.status == "fail"


def test_a_destructive_tidy_flips_polarity_and_is_counted():
    def drop_negations(text):
        return " ".join(w for w in text.split() if w.lower() not in ("not", "never"))

    report = evaluate(System("bad", tidy=drop_negations), ["disfluent_en"], repeats=1)
    flips = gates(report)["disfluent_en.polarity_flips"]
    assert flips.value > 0 and flips.status == "fail"
    assert report.detail["disfluent_en"]["polarity_flip_ids"]


def test_an_over_eager_tidy_fails_the_unnecessary_edit_rate():
    report = evaluate(System("shouty", tidy=str.lower), ["clean_en"], repeats=1)
    g = gates(report)["clean_en.uer"]
    assert g.value > 0.9 and g.status == "fail"
    assert report.detail["clean_en"]["examples"]


def test_reference_rule_baseline():
    report = evaluate(text_suite.reference_system(), ["clean_en", "disfluent_en"], repeats=1)
    g = gates(report)
    assert g["disfluent_en.polarity_flips"].value == 0
    assert g["disfluent_en.reduction"].status == "pass"
    by_category = report.detail["disfluent_en"]["by_category"]
    assert by_category["repeat_k3"]["exact"] == by_category["repeat_k3"]["n"]
    assert by_category["negation_trap"]["exact"] == by_category["negation_trap"]["n"]
    assert by_category["self_repair"]["exact"] == 0
    assert report.detail["disfluent_en"]["system_check_reject_rate"] == 0


@pytest.mark.parametrize(
    ("heard", "expected"),
    [
        ("Um, I think we should go.", "I think we should go."),
        ("We should, uh, ship it.", "We should, ship it."),
        ("I I think so.", "I think so."),
        ("We need to we need to go.", "We need to go."),
        ("It is very very cold.", "It is very very cold."),
        ("Do not merge.", "Do not merge."),
        ("Um.", ""),
        ("", ""),
    ],
)
def test_reference_rule_examples(heard, expected):
    assert reference_rule(heard) == expected


def test_gate_decision_directions():
    assert Gate("s", "m", 1.0, 1.0, "<=").decide().status == "pass"
    assert Gate("s", "m", 1.0, 1.0, "<").decide().status == "fail"
    assert Gate("s", "m", 1.0, 1.0, ">=").decide().status == "pass"
    assert Gate("s", "m", 0.9, 1.0, ">=").decide().status == "fail"
    assert Gate("s", "m", None, 1.0, ">=").decide().status == "pending"


def test_load_system_builtins_and_plugins(monkeypatch):
    assert text_suite.load_system("identity").name == "identity"
    assert text_suite.load_system("reference-rule").tidy is reference_rule
    with pytest.raises(SystemExit, match="unknown system"):
        text_suite.load_system("nonsense")
    with pytest.raises(SystemExit, match="did not return"):
        text_suite.load_system("builtins:dict")
    assert text_suite.load_system("myna.benchmarker.text_suite:identity_system").tidy


def _args(tmp_path, **overrides):
    values = {
        "system": "identity",
        "suite": None,
        "repeats": 1,
        "json_out": None,
        "strict": False,
    }
    return argparse.Namespace(**{**values, **overrides})


def test_cmd_text_exits_nonzero_on_a_failed_gate_and_writes_json(tmp_path, capsys):
    out = tmp_path / "report.json"
    with pytest.raises(SystemExit) as raised:
        text_suite.cmd_text(_args(tmp_path, json_out=str(out)))
    assert raised.value.code == 1
    assert "FAIL" in capsys.readouterr().out
    saved = json.loads(out.read_text(encoding="utf-8"))
    assert saved["system"] == "identity"
    assert {g["status"] for g in saved["gates"]} == {"pass", "fail", "pending"}


def test_cmd_text_passes_when_nothing_failed_and_strict_rejects_pending(capsys):
    text_suite.cmd_text(_args(None, suite=["clean_en"]))
    assert "PASS" in capsys.readouterr().out
    with pytest.raises(SystemExit) as raised:
        text_suite.cmd_text(_args(None, suite=["translate_en_fr"], strict=True))
    assert raised.value.code == 1
    text_suite.cmd_text(_args(None, suite=["translate_en_fr"]))
