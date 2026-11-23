"""`myna-bench text`: gates, pending, plug-in systems and the CLI exit status."""

from __future__ import annotations

import argparse
import json

import pytest

from myna.benchmarker import text_corpus, text_suite
from myna.benchmarker.text_gate import GateConfig
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
    lookup = {r.en: r.target for r in rows}
    good = evaluate(System("oracle", translate=lookup.__getitem__), ["translate_en_fr"], 1)
    assert [g.status for g in good.gates] == ["pass", "pass"]
    assert gates(good)["translate_en_fr.chrf"].value == pytest.approx(1.0)
    bad = evaluate(System("echo", translate=lambda text: text), ["translate_en_fr"], 1)
    assert gates(bad)["translate_en_fr.chrf"].status == "fail"


def test_translation_p95_gate_fails_a_slow_system(monkeypatch):
    ticks = iter(range(0, 10**7, 1))
    monkeypatch.setattr(text_suite.time, "perf_counter", lambda: next(ticks) * 0.5)
    lookup = {r.en: r.target for r in text_corpus.load().translate_en_fr}
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
        "plugin": None,
        "socket": None,
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


@pytest.fixture(scope="module")
def corpus():
    return text_corpus.load()


def test_clean_detail_counts_every_changed_sentence(corpus):
    first = {c.text for c in corpus.clean_en[:7]}
    system = System("edits-seven", tidy=lambda t: t.upper() if t in first else t)
    report = evaluate(system, ["clean_en"], repeats=2)
    detail = report.detail["clean_en"]
    assert detail["n"] == 200 and detail["changed"] == 7
    assert detail["char_edits"] == sum(
        1
        for c in corpus.clean_en[:7]
        for a, b in zip(c.text, c.text.upper(), strict=True)
        if a != b
    )
    assert [e["id"] for e in detail["examples"]] == [c.id for c in corpus.clean_en[:5]]
    assert detail["examples"][0]["output"] == corpus.clean_en[0].text.upper()
    assert set(detail["latency_ms"]) == {"p50", "p95"}
    uer = gates(report)["clean_en.uer"]
    assert uer.value == pytest.approx(0.035) and uer.status == "fail"


def test_one_changed_sentence_in_two_hundred_is_within_the_gate(corpus):
    target = corpus.clean_en[0].text
    system = System("one", tidy=lambda t: t + "!" if t == target else t)
    uer = gates(evaluate(system, ["clean_en"], repeats=1))["clean_en.uer"]
    assert uer.value == pytest.approx(0.005) and uer.status == "pass"


def test_an_oracle_tidy_scores_every_disfluent_metric_perfectly(corpus):
    oracle = {p.heard: p.intended for p in corpus.disfluent_en}
    report = evaluate(System("oracle", tidy=oracle.__getitem__), ["disfluent_en"], repeats=1)
    g, detail = gates(report), report.detail["disfluent_en"]
    assert g["disfluent_en.reduction"].value == 1.0
    assert g["disfluent_en.polarity_flips"].value == 0
    assert detail["n"] == 190 and detail["exact"] == 190
    assert detail["after_distance"] == 0 and detail["heard_distance"] > 0
    assert detail["system_check_reject_rate"] == 0
    assert detail["polarity_flip_ids"] == []
    for row in detail["by_category"].values():
        assert row["exact"] == row["n"]


def test_identity_tidy_only_gets_the_untouched_categories_right(corpus):
    report = evaluate(text_suite.identity_system(), ["disfluent_en"], repeats=1)
    detail = report.detail["disfluent_en"]
    keep = {"reduplication_keep", "ambiguous_keep", "negation_trap"}
    assert detail["after_distance"] == detail["heard_distance"]
    assert detail["exact"] == sum(p.category in keep for p in corpus.disfluent_en)
    for name, row in detail["by_category"].items():
        assert row["n"] == sum(p.category == name for p in corpus.disfluent_en)
        assert row["reduction"] == (None if name in keep else 0.0)
        assert row["exact"] == (row["n"] if name in keep else 0)


def test_a_tidy_that_breaks_the_check_is_counted_apart_from_the_gate(corpus):
    report = evaluate(System("wipe", tidy=lambda t: ""), ["disfluent_en"], repeats=1)
    detail = report.detail["disfluent_en"]
    assert detail["system_check_reject_rate"] == 1.0
    assert gates(report)["disfluent_en.check_false_reject"].value == 0


def test_a_negation_invented_by_the_system_is_a_flip():
    report = evaluate(System("invents", tidy=lambda t: "Do not " + t), ["disfluent_en"], repeats=1)
    assert gates(report)["disfluent_en.polarity_flips"].value > 100


def test_translation_detail_and_latency_percentiles(monkeypatch, corpus):
    clock = iter(x for i in range(10**6) for x in (0.0, 0.0125))
    monkeypatch.setattr(text_suite.time, "perf_counter", lambda: next(clock))
    lookup = {r.en: r.target for r in corpus.translate_en_fr}
    report = evaluate(System("t", translate=lookup.__getitem__), ["translate_en_fr"], repeats=2)
    detail = report.detail["translate_en_fr"]
    assert detail["n"] == 200
    assert detail["latency_ms"] == {"p50": pytest.approx(12.5), "p95": pytest.approx(12.5)}
    g = gates(report)
    assert g["translate_en_fr.p95_ms"].value == pytest.approx(12.5)
    assert g["translate_en_fr.chrf"].limit == 0.58 and g["translate_en_fr.p95_ms"].limit == 400


def test_the_ship_gates_are_the_decided_ones():
    assert text_suite.UER_MAX == 0.01
    assert text_suite.REDUCTION_MIN == 0.5
    assert text_suite.FALSE_REJECT_MAX == 0.05
    assert text_suite.CHRF_MIN == 0.58
    assert text_suite.P95_MAX_MS == 400.0
    g = gates(evaluate(text_suite.identity_system(), ALL, repeats=1))
    assert {k: (v.limit, v.direction) for k, v in g.items()} == {
        "clean_en.uer": (0.01, "<="),
        "disfluent_en.reduction": (0.5, ">="),
        "disfluent_en.polarity_flips": (0, "<="),
        "disfluent_en.check_false_reject": (0.05, "<="),
        "translate_en_fr.chrf": (0.58, ">="),
        "translate_en_fr.p95_ms": (400.0, "<"),
    }


def test_the_report_text_and_json_carry_every_gate():
    report = evaluate(text_suite.identity_system(), ALL, repeats=1)
    lines = text_suite.format_report(report).splitlines()
    assert lines[0] == "system: identity"
    assert lines[1] == "  PASS    clean_en.uer = 0  (gate <= 0.01)"
    assert lines[2] == "  FAIL    disfluent_en.reduction = 0  (gate >= 0.5)"
    assert lines[-1] == "  PENDING translate_en_fr.p95_ms = -  (gate < 400)"
    saved = text_suite.to_json(report)
    assert saved["system"] == "identity"
    assert saved["gates"][0] == {
        "suite": "clean_en",
        "metric": "uer",
        "value": 0.0,
        "limit": 0.01,
        "direction": "<=",
        "status": "pass",
    }
    assert set(saved["detail"]) == set(ALL)
    assert saved["detail"]["translate_en_fr"] == {}


def test_arguments_default_to_identity_over_every_suite():
    import argparse

    parser = argparse.ArgumentParser()
    text_suite.add_arguments(parser)
    args = parser.parse_args([])
    assert (args.system, args.suite, args.repeats) == ("identity", None, 3)
    assert (args.json_out, args.strict, args.plugin, args.socket) == (None, False, None, None)
    args = parser.parse_args(["--suite", "clean_en", "--suite", "translate_en_fr", "--strict"])
    assert args.suite == ["clean_en", "translate_en_fr"] and args.strict
    with pytest.raises(SystemExit, match="unknown suite"):
        evaluate(text_suite.identity_system(), ["nope"], repeats=1)


def test_cmd_text_runs_every_suite_by_default(capsys):
    with pytest.raises(SystemExit):
        text_suite.cmd_text(_args(None, repeats=1))
    out = capsys.readouterr().out
    for name in ALL:
        assert name in out


def test_reference_rule_restitching():
    assert reference_rule("um, we should go") == "we should go"
    assert reference_rule("Um, we should go") == "We should go"
    assert reference_rule('He said, "I I think so."') == 'He said, "I think so."'
    assert reference_rule("We went, we went home.") == "We went home."
    assert reference_rule("Uh, the the end.") == "The end."
    assert reference_rule("We need to go, uh, now") == "We need to go, now"
    assert reference_rule("A b A b c") == "A b c"
    assert reference_rule("go go go go") == "go"
    assert reference_rule("Really really, it works") == "Really really, it works"
    assert reference_rule("Ok ok.") == "Ok."


def test_pending_gates_still_state_their_limits():
    g = gates(evaluate(System("nothing"), ALL, repeats=1))
    pending = {k: (v.limit, v.direction) for k, v in g.items() if v.status == "pending"}
    assert pending == {
        "clean_en.uer": (0.01, "<="),
        "disfluent_en.reduction": (0.5, ">="),
        "disfluent_en.polarity_flips": (0, "<="),
        "translate_en_fr.chrf": (0.58, ">="),
        "translate_en_fr.p95_ms": (400.0, "<"),
    }


def test_false_reject_rate_is_the_share_of_gold_edits_rejected(monkeypatch, corpus):
    edits = [p for p in corpus.disfluent_en if p.heard != p.intended]
    doomed = {p.heard for p in edits[:5]}
    monkeypatch.setattr(
        text_suite,
        "conservative_edit_violation",
        lambda source, output: "bad" if source in doomed and output != source else None,
    )
    report = evaluate(System("none"), ["disfluent_en"], repeats=1)
    rate = gates(report)["disfluent_en.check_false_reject"]
    assert rate.value == pytest.approx(5 / len(edits)) and rate.status == "pass"
    assert report.detail["disfluent_en"]["gold_rejected"] == [p.id for p in edits[:5]]
    monkeypatch.setattr(text_suite, "conservative_edit_violation", lambda s, o: "bad")
    rate = gates(evaluate(System("none"), ["disfluent_en"], repeats=1))
    assert rate["disfluent_en.check_false_reject"].value == 1.0
    assert rate["disfluent_en.check_false_reject"].status == "fail"


def test_latency_summary_is_p50_and_p95():
    assert text_suite._latency([float(i) for i in range(1, 101)]) == {"p50": 50.0, "p95": 95.0}


def test_the_json_report_is_indented(tmp_path):
    out = tmp_path / "r.json"
    text_suite.cmd_text(_args(tmp_path, system="reference-rule", json_out=str(out)))
    assert out.read_text(encoding="utf-8").startswith('{\n  "system": "reference-rule"')


def test_repeats_set_the_number_of_warm_passes():
    calls = []

    def tidy(text):
        calls.append(text)
        return text

    evaluate(System("count", tidy=tidy), ["clean_en"], repeats=2)
    assert len(calls) == 200 * (1 + 2)


def _de_lookup():
    return {r.en: r.target for r in text_corpus.load().pairs["de"]}


def test_a_pair_slice_gets_the_plugin_gate_and_the_degeneracy_gates():
    gate = GateConfig(vendor_comet=0.85)
    system = System("oracle", translate=_de_lookup().__getitem__)
    report = evaluate(system, ["translate_en_de"], 1, {"translate_en_de": gate})
    g = gates(report)
    assert set(g) == {
        f"translate_en_de.{m}"
        for m in (
            "chrf p95_ms empty_rate repeat_loop_rate length_ratio_min length_ratio_max "
            "copy_through_rate vendor_comet"
        ).split()
    }
    assert all(x.status == "pass" for x in report.gates)
    assert g["translate_en_de.chrf"].limit == 0.40 and g["translate_en_de.chrf"].value == 1.0
    assert g["translate_en_de.vendor_comet"].value == 0.85
    assert g["translate_en_de.copy_through_rate"].limit == 0.05
    hyps = report.detail["translate_en_de"]["hypotheses"]
    assert len(hyps) == 200 and hyps[0]["hypothesis"] == _de_lookup()[hyps[0]["source"]]


def test_a_pair_without_a_plugin_config_gets_the_two_legacy_gates():
    system = System("oracle", translate=_de_lookup().__getitem__)
    report = evaluate(system, ["translate_en_de"], 1)
    assert [g.metric for g in report.gates] == ["chrf", "p95_ms"]
    assert gates(report)["translate_en_de.chrf"].limit == 0.58
    assert "hypotheses" not in report.detail["translate_en_de"]


@pytest.mark.parametrize(
    ("translate", "metric"),
    [
        (lambda t: "", "empty_rate"),
        (lambda t: "la " * 12, "repeat_loop_rate"),
        (lambda t: t, "copy_through_rate"),
        (lambda t: "x", "length_ratio_min"),
        (lambda t: t * 6, "length_ratio_max"),
    ],
)
def test_each_degeneracy_gate_fails_its_own_failure_mode(translate, metric):
    report = evaluate(
        System("bad", translate=translate),
        ["translate_en_de"],
        1,
        {"translate_en_de": GateConfig()},
    )
    assert gates(report)[f"translate_en_de.{metric}"].status == "fail"


def test_a_low_vendor_comet_fails_the_gate_and_none_adds_no_gate():
    system = System("oracle", translate=_de_lookup().__getitem__)
    low = evaluate(
        system, ["translate_en_de"], 1, {"translate_en_de": GateConfig(vendor_comet=0.79)}
    )
    assert gates(low)["translate_en_de.vendor_comet"].status == "fail"
    none = evaluate(system, ["translate_en_de"], 1, {"translate_en_de": GateConfig()})
    assert "translate_en_de.vendor_comet" not in gates(none)


def test_a_reverse_plugin_translates_the_foreign_side_into_english():
    lookup = {r.target: r.en for r in text_corpus.load().pairs["de"]}
    report = evaluate(
        System("oracle", translate=lookup.__getitem__),
        ["translate_en_de"],
        1,
        {"translate_en_de": GateConfig(reverse=True)},
    )
    assert gates(report)["translate_en_de.chrf"].value == 1.0


def test_a_system_without_translate_leaves_every_pair_gate_pending():
    report = evaluate(
        text_suite.identity_system(),
        ["translate_en_de"],
        1,
        {"translate_en_de": GateConfig(vendor_comet=0.9)},
    )
    g = gates(report)
    assert report.pending and not report.failed
    assert g["translate_en_de.vendor_comet"].status == "pass"
    assert {x.status for k, x in g.items() if not k.endswith("vendor_comet")} == {"pending"}


def test_a_pair_with_no_slice_says_how_to_build_it():
    with pytest.raises(SystemExit, match="--pair en:xx"):
        evaluate(System("t", translate=str), ["translate_en_xx"], 1)


def test_plugin_plan_picks_the_slice_and_the_gate_of_the_plugin_file():
    suites, plan = text_suite.plugin_plan(
        {"task": "text.translate", "language": "en", "to": "de", "gate": {"p95_max_ms": 200}}
    )
    assert suites == ["translate_en_de"] and plan["translate_en_de"].p95_max_ms == 200
    assert text_suite.plugin_plan({"task": "text.cleanup"}) == (["clean_en", "disfluent_en"], {})


def test_a_pair_with_no_slice_is_probed_instead():
    suites, plan = text_suite.plugin_plan(
        {"task": "text.translate", "language": "af", "to": "en", "gate": {"vendor_comet": 0.85}}
    )
    assert suites == ["probe_af"] and plan["probe_af"].vendor_comet == 0.85


def test_a_probe_passes_a_model_that_answers_and_fails_one_that_does_not():
    ok = gates(evaluate(System("t", translate=lambda t: f"[{t}]"), ["probe_af"], 1))
    assert set(ok) == {"probe_af.p95_ms", "probe_af.empty_rate", "probe_af.repeat_loop_rate"}
    assert {g.status for g in ok.values()} == {"pass"}
    silent = gates(evaluate(System("t", translate=lambda t: ""), ["probe_af"], 1))
    assert silent["probe_af.empty_rate"].status == "fail"
    looping = gates(evaluate(System("t", translate=lambda t: "la " * 60), ["probe_af"], 1))
    assert looping["probe_af.repeat_loop_rate"].status == "fail"
    low = evaluate(
        System("t", translate=str), ["probe_af"], 1, {"probe_af": GateConfig(vendor_comet=0.7)}
    )
    assert gates(low)["probe_af.vendor_comet"].status == "fail"
    assert low.detail["probe_af"]["probe"] is True
    assert low.detail["probe_af"]["n"] == 200
    idle = gates(evaluate(System("t"), ["probe_af"], 1))
    assert {g.status for g in idle.values()} == {"pending"}
    into_english = evaluate(
        System("t", translate=lambda t: "la " * 60),
        ["probe_af"],
        1,
        {"probe_af": GateConfig(reverse=True)},
    )
    assert set(gates(into_english)) == {"probe_af.p95_ms", "probe_af.empty_rate"}


def test_cmd_text_with_a_plugin_and_a_socket_scores_that_model(tmp_path, capsys, monkeypatch):
    plugin = tmp_path / "translate-en-de.toml"
    plugin.write_text(
        'id = "m"\ntask = "text.translate"\nlanguage = "en"\nto = "de"\n[gate]\nchrf_min = 0.9\n',
        encoding="utf-8",
    )
    lookup = _de_lookup()
    monkeypatch.setattr(
        text_suite,
        "socket_system",
        lambda path, model: System(f"socket:{model}@{path}", translate=lookup.__getitem__),
    )
    text_suite.cmd_text(_args(tmp_path, plugin=str(plugin), socket="/run/x.sock"))
    out = capsys.readouterr().out
    assert "system: socket:m@/run/x.sock" in out
    assert "PASS    translate_en_de.chrf = 1  (gate >= 0.9)" in out


def test_cmd_text_socket_without_a_plugin_is_refused(tmp_path):
    with pytest.raises(SystemExit, match="--plugin"):
        text_suite.cmd_text(_args(tmp_path, socket="/run/x.sock"))
