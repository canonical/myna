"""``myna-bench text``: the decision-gated text evaluation suite.

Scores a text system (a cleanup step, a translator) on the vendored corpus
(``text_corpus``) against the ship gates of the processing-graph decisions:

``clean_en``        unnecessary edit rate (UER) <= 1%: the share of sentences
                    that were already clean and came back changed in any
                    character, case and punctuation included.
``disfluent_en``    case+punct edit-distance reduction >= 50% (full Tidy),
                    zero polarity flips, ``Check::ConservativeEdit`` false-
                    reject rate <= 5% on the corpus's own gold edits.
``translate_en_fr`` corpus chrF >= 0.58 and warm p95 latency < 400 ms.
``translate_en_<l>`` any other pair slice; its gate is the plugin's
                    ``[gate]`` table (``--plugin``, see ``text_gate``): chrF,
                    p95, and the degeneracy rates.

A gate on a task the system does not offer is ``pending``, never a pass. A
system is an object with optional ``tidy(text) -> str`` and
``translate(text) -> str``; ``--system module:factory`` loads one, so a model
adapter plugs in without touching this file. Exit status is 1 on any failed
gate, and 1 on a pending one with ``--strict``.
"""

from __future__ import annotations

import argparse
import importlib
import json
import sys
import time
from collections import defaultdict
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

from myna.benchmarker import text_corpus, text_eval
from myna.benchmarker.text_gate import GateConfig, find_plugin, load_plugin, pair_of, socket_system
from myna.benchmarker.text_metrics import (
    chrf,
    conservative_edit_violation,
    copy_through_rate,
    edit_distance,
    empty_rate,
    median_length_ratio,
    percentile,
    polarity_flip,
    repeat_loop_rate,
)

UER_MAX = 0.01
REDUCTION_MIN = 0.50
FALSE_REJECT_MAX = 0.05
CHRF_MIN = 0.58
P95_MAX_MS = 400.0

Fn = Callable[[str], str]


@dataclass
class System:
    name: str
    tidy: Fn | None = None
    translate: Fn | None = None


@dataclass
class Gate:
    suite: str
    metric: str
    value: float | None
    limit: float
    direction: str  # "<=", ">=" or "<"
    status: str = "pending"

    def decide(self) -> Gate:
        if self.value is None:
            self.status = "pending"
        else:
            ok = {
                "<=": self.value <= self.limit,
                ">=": self.value >= self.limit,
                "<": self.value < self.limit,
            }[self.direction]
            self.status = "pass" if ok else "fail"
        return self


@dataclass
class Report:
    system: str
    gates: list[Gate] = field(default_factory=list)
    detail: dict[str, Any] = field(default_factory=dict)

    @property
    def failed(self) -> bool:
        return any(g.status == "fail" for g in self.gates)

    @property
    def pending(self) -> bool:
        return any(g.status == "pending" for g in self.gates)


def identity_system() -> System:
    """No cleanup and no translation: the floor every other system must beat."""
    return System("identity", tidy=lambda text: text)


# Words a speaker repeats on purpose. Deliberately a short, general list, not
# one fitted to the corpus: a rule tuned on the eval set would grade itself.
_REDUPLICATION = frozenset({"very", "so", "really", "had", "that", "bye", "no", "way"})
_PAUSES = frozenset({"um", "uh", "erm", "er", "uhm"})


def _bare(token: str) -> str:
    return token.strip('.,;:!?"()').casefold()


def reference_rule(text: str) -> str:
    """A small classical rule used as the harness's non-trivial baseline.

    Drops unambiguous vocalized pauses and the first copy of a tandem repeat of
    one to four words, restitching case and commas. It is a reference for the
    harness, not the shipping Rust rule (``classical-disfluency-rule``)."""
    tokens = [t for t in text.split() if _bare(t) not in _PAUSES]
    changed = True
    while changed:
        changed = False
        for k in range(4, 0, -1):
            for start in range(len(tokens) - 2 * k + 1):
                first = [_bare(t) for t in tokens[start : start + k]]
                second = [_bare(t) for t in tokens[start + k : start + 2 * k]]
                if first != second or (k == 1 and first[0] in _REDUPLICATION):
                    continue
                tokens = _drop_second_copy(tokens, start, k)
                changed = True
                break
            if changed:
                break
    if not tokens:
        return ""
    return " ".join(_restitch(text, tokens))


_TRAILING = ".,;:!?\"')"


def _drop_second_copy(tokens: list[str], start: int, k: int) -> list[str]:
    """Remove the repeat's second copy, keeping the first copy's casing and
    opening quote, and take the second copy's closing punctuation."""
    kept = list(tokens[: start + k])
    second_last = tokens[start + 2 * k - 1]
    trailing = second_last[len(second_last.rstrip(_TRAILING)) :]
    kept[-1] = kept[-1].rstrip(_TRAILING) + trailing
    return kept + tokens[start + 2 * k :]


def _restitch(original: str, tokens: list[str]) -> list[str]:
    """Capitalise a sentence start that lost its first word and trim a comma
    left dangling at either end."""
    if not tokens:
        return tokens
    if original[:1].isupper() and tokens[0][:1].islower():
        tokens[0] = tokens[0][:1].upper() + tokens[0][1:]
    tokens[0] = tokens[0].lstrip(",")
    return tokens


def reference_system() -> System:
    return System("reference-rule", tidy=reference_rule)


BUILTIN: dict[str, Callable[[], System]] = {
    "identity": identity_system,
    "reference-rule": reference_system,
}


def load_system(spec: str) -> System:
    if spec in BUILTIN:
        return BUILTIN[spec]()
    module_name, _, attr = spec.partition(":")
    if not attr:
        raise SystemExit(f"unknown system {spec!r}: one of {sorted(BUILTIN)} or module:factory")
    system = getattr(importlib.import_module(module_name), attr)()
    if not isinstance(system, System):
        raise SystemExit(f"{spec} did not return a myna.benchmarker.text_suite.System")
    return system


def _timed(fn: Fn, texts: list[str], repeats: int) -> tuple[list[str], list[float]]:
    """Outputs of one pass and per-call milliseconds of ``repeats`` warm passes."""
    outputs = [fn(t) for t in texts]  # warm-up
    millis: list[float] = []
    for _ in range(repeats):
        for text in texts:
            started = time.perf_counter()
            fn(text)
            millis.append((time.perf_counter() - started) * 1000)
    return outputs, millis


def eval_clean(
    system: System, corpus: text_corpus.TextCorpus, repeats: int
) -> tuple[list[Gate], dict[str, Any]]:
    if system.tidy is None:
        return [Gate("clean_en", "uer", None, UER_MAX, "<=").decide()], {}
    texts = [c.text for c in corpus.clean_en]
    outputs, millis = _timed(system.tidy, texts, repeats)
    changed = [i for i, (a, b) in enumerate(zip(texts, outputs, strict=True)) if a != b]
    char_edits = sum(edit_distance(a, b) for a, b in zip(texts, outputs, strict=True))
    uer = len(changed) / len(texts)
    detail = {
        "n": len(texts),
        "changed": len(changed),
        "char_edits": char_edits,
        "examples": [{"id": corpus.clean_en[i].id, "output": outputs[i]} for i in changed[:5]],
        "latency_ms": _latency(millis),
    }
    return [Gate("clean_en", "uer", uer, UER_MAX, "<=").decide()], detail


def eval_disfluent(
    system: System, corpus: text_corpus.TextCorpus, repeats: int
) -> tuple[list[Gate], dict[str, Any]]:
    pairs = corpus.disfluent_en
    edits = [p for p in pairs if p.heard != p.intended]
    rejected = [p for p in edits if conservative_edit_violation(p.heard, p.intended)]
    false_reject = len(rejected) / len(edits)
    gold_gate = Gate("disfluent_en", "check_false_reject", false_reject, FALSE_REJECT_MAX, "<=")
    detail: dict[str, Any] = {
        "n": len(pairs),
        "gold_rejected": [p.id for p in rejected],
        "heard_distance": sum(edit_distance(p.heard, p.intended) for p in pairs),
    }
    if system.tidy is None:
        return [
            Gate("disfluent_en", "reduction", None, REDUCTION_MIN, ">=").decide(),
            Gate("disfluent_en", "polarity_flips", None, 0, "<=").decide(),
            gold_gate.decide(),
        ], detail
    outputs, millis = _timed(system.tidy, [p.heard for p in pairs], repeats)
    before = detail["heard_distance"]
    after = sum(edit_distance(o, p.intended) for o, p in zip(outputs, pairs, strict=True))
    flips = [
        p.id for o, p in zip(outputs, pairs, strict=True) if polarity_flip(p.heard, p.intended, o)
    ]
    system_rejects = sum(
        1
        for o, p in zip(outputs, pairs, strict=True)
        if conservative_edit_violation(p.heard, o) is not None
    )
    per_category: dict[str, list[int]] = defaultdict(lambda: [0, 0, 0])
    for o, p in zip(outputs, pairs, strict=True):
        row = per_category[p.category]
        row[0] += edit_distance(p.heard, p.intended)
        row[1] += edit_distance(o, p.intended)
        row[2] += o == p.intended
    detail.update(
        {
            "after_distance": after,
            "exact": sum(o == p.intended for o, p in zip(outputs, pairs, strict=True)),
            "polarity_flip_ids": flips,
            "system_check_reject_rate": system_rejects / len(pairs),
            "by_category": {
                c: {
                    "n": sum(1 for p in pairs if p.category == c),
                    "reduction": (1 - r[1] / r[0]) if r[0] else None,
                    "exact": r[2],
                }
                for c, r in sorted(per_category.items())
            },
            "latency_ms": _latency(millis),
        }
    )
    reduction = 1 - after / before
    return [
        Gate("disfluent_en", "reduction", reduction, REDUCTION_MIN, ">=").decide(),
        Gate("disfluent_en", "polarity_flips", float(len(flips)), 0, "<=").decide(),
        gold_gate.decide(),
    ], detail


def eval_translate(
    system: System,
    corpus: text_corpus.TextCorpus,
    repeats: int,
    lang: str = "fr",
    gate: GateConfig | None = None,
) -> tuple[list[Gate], dict[str, Any]]:
    suite = text_corpus.pair_slice(lang)
    chrf_limit = gate.chrf_limit() if gate else CHRF_MIN
    p95_limit = gate.p95_max_ms if gate else P95_MAX_MS
    gates = [Gate(suite, "chrf", None, chrf_limit, ">=")]
    gates.append(Gate(suite, "p95_ms", None, p95_limit, "<"))
    if gate:
        gates += [
            Gate(suite, "empty_rate", None, gate.empty_max, "<="),
            Gate(suite, "repeat_loop_rate", None, gate.repeat_max, "<="),
            Gate(suite, "length_ratio_min", None, gate.ratio_min, ">="),
            Gate(suite, "length_ratio_max", None, gate.ratio_max, "<="),
            Gate(suite, "copy_through_rate", None, gate.copy_max, "<="),
        ]
    if gate and gate.vendor_comet is not None:
        gates.append(Gate(suite, "vendor_comet", gate.vendor_comet, gate.comet_min, ">="))
    if system.translate is None:
        return [g.decide() for g in gates], {}
    rows = corpus.pairs.get(lang)
    if rows is None:
        raise SystemExit(f"no corpus slice {suite}: myna-bench build-text-corpus --pair en:{lang}")
    sources = [r.target if gate and gate.reverse else r.en for r in rows]
    refs = [r.en if gate and gate.reverse else r.target for r in rows]
    outputs, millis = _timed(system.translate, sources, repeats)
    latency = _latency(millis)
    ratio = median_length_ratio(outputs, refs)
    values = {
        "chrf": chrf(outputs, refs),
        "p95_ms": latency["p95"],
        "empty_rate": empty_rate(outputs),
        "repeat_loop_rate": repeat_loop_rate(outputs),
        "length_ratio_min": ratio,
        "length_ratio_max": ratio,
        "copy_through_rate": copy_through_rate(outputs, sources),
    }
    for g in gates:
        if g.metric in values:
            g.value = values[g.metric]
        g.decide()
    detail: dict[str, Any] = {"n": len(rows), "latency_ms": latency}
    if gate:
        detail["vendor_comet"] = gate.vendor_comet
        detail["hypotheses"] = [
            {"id": r.id, "source": s, "hypothesis": o}
            for r, s, o in zip(rows, sources, outputs, strict=True)
        ]
    return gates, detail


PROBE_PREFIX = "probe_"


def eval_probe(
    system: System,
    corpus: text_corpus.TextCorpus,
    repeats: int,
    lang: str,
    gate: GateConfig,
) -> tuple[list[Gate], dict[str, Any]]:
    """A pair with no corpus slice: it cannot be scored, only shown to serve.

    The English clean_en sentences go in whatever the source language, so the
    output is not judged for meaning, only for being there and in time, and
    loop-free when English is the model's own source; the vendor COMET is the
    one quality figure, when Mozilla gives one."""
    suite = PROBE_PREFIX + lang
    texts = [c.text for c in corpus.clean_en]
    outputs, millis = _timed(system.translate, texts, repeats) if system.translate else ([], [])
    latency = _latency(millis)
    measured = bool(outputs)
    gates = [
        Gate(suite, "p95_ms", latency["p95"] if measured else None, gate.p95_max_ms, "<"),
        Gate(suite, "empty_rate", empty_rate(outputs) if measured else None, gate.empty_max, "<="),
    ]
    if not gate.reverse:
        # A model into English fed English is out of its language: its loops
        # say nothing about the language it is for.
        gates.append(
            Gate(
                suite,
                "repeat_loop_rate",
                repeat_loop_rate(outputs) if measured else None,
                gate.repeat_max,
                "<=",
            )
        )
    if gate.vendor_comet is not None:
        gates.append(Gate(suite, "vendor_comet", gate.vendor_comet, gate.comet_min, ">="))
    detail: dict[str, Any] = {
        "n": len(texts),
        "latency_ms": latency,
        "vendor_comet": gate.vendor_comet,
        "probe": True,
        "examples": [
            {"source": t, "output": o} for t, o in zip(texts[:5], outputs[:5], strict=False)
        ],
    }
    return [g.decide() for g in gates], detail


def _latency(millis: list[float]) -> dict[str, float]:
    return {"p50": percentile(millis, 50), "p95": percentile(millis, 95)}


SUITES = {"clean_en": eval_clean, "disfluent_en": eval_disfluent, "translate_en_fr": eval_translate}


def _run_suite(
    name: str,
    system: System,
    corpus: text_corpus.TextCorpus,
    repeats: int,
    gate: GateConfig | None,
) -> tuple[list[Gate], dict[str, Any]]:
    if name in ("clean_en", "disfluent_en"):
        return SUITES[name](system, corpus, repeats)
    if name.startswith(PROBE_PREFIX):
        return eval_probe(system, corpus, repeats, name[len(PROBE_PREFIX) :], gate or GateConfig())
    match = text_corpus.PAIR_RE.fullmatch(name)
    if not match:
        raise SystemExit(f"unknown suite {name!r}")
    return eval_translate(system, corpus, repeats, match[1], gate)


def evaluate(
    system: System,
    suites: list[str],
    repeats: int = 3,
    gates: dict[str, GateConfig] | None = None,
) -> Report:
    corpus = text_corpus.load()
    report = Report(system.name)
    for name in suites:
        suite_gates, detail = _run_suite(name, system, corpus, repeats, (gates or {}).get(name))
        report.gates.extend(suite_gates)
        report.detail[name] = detail
    return report


def format_report(report: Report) -> str:
    lines = [f"system: {report.system}"]
    for g in report.gates:
        value = "-" if g.value is None else f"{g.value:.4g}"
        verdict = f"(gate {g.direction} {g.limit:g})"
        lines.append(f"  {g.status.upper():7} {g.suite}.{g.metric} = {value}  {verdict}")
    return "\n".join(lines)


def to_json(report: Report) -> dict[str, Any]:
    return {
        "system": report.system,
        "gates": [
            {
                "suite": g.suite,
                "metric": g.metric,
                "value": g.value,
                "limit": g.limit,
                "direction": g.direction,
                "status": g.status,
            }
            for g in report.gates
        ],
        "detail": report.detail,
    }


def plugin_plan(plugin: dict[str, Any]) -> tuple[list[str], dict[str, GateConfig]]:
    """The suites a plugin is graded on and the gate config of its pair slice."""
    if plugin.get("task") == "text.cleanup":
        return ["clean_en", "disfluent_en"], {}
    lang = pair_of(plugin)
    suite = text_corpus.pair_slice(lang)
    if not (text_corpus.CORPUS_DIR / f"{suite}.jsonl").is_file():
        suite = PROBE_PREFIX + lang
    return [suite], {suite: GateConfig.from_plugin(plugin)}


def add_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument(
        "--system",
        default="identity",
        help="identity, reference-rule or module:factory returning a System (default identity)",
    )
    parser.add_argument(
        "--suite",
        action="append",
        help="suite to run: clean_en, disfluent_en or translate_en_<l> (repeatable; "
        "default clean_en, disfluent_en and translate_en_fr)",
    )
    parser.add_argument(
        "--plugin",
        help="a plugin file or id: grade it on its own slice with the gate config of its file",
    )
    parser.add_argument(
        "--socket", help="with --plugin: score the plugin's model on this running provider socket"
    )
    parser.add_argument("--repeats", type=int, default=3, help="warm latency passes (default 3)")
    parser.add_argument("--json", dest="json_out", help="also write the full report here")
    parser.add_argument("--strict", action="store_true", help="treat a pending gate as a failure")
    text_eval.add_arguments(parser)


def cmd_text(args: argparse.Namespace) -> None:
    if getattr(args, "eval_run", None):
        text_eval.cmd_eval(args)
        return
    gates: dict[str, GateConfig] = {}
    suites = args.suite or sorted(SUITES)
    system_spec = args.system
    system: System | None = None
    if args.plugin:
        from pathlib import Path

        repo = Path(__file__).resolve().parents[4]
        plugin = load_plugin(find_plugin(repo, args.plugin))
        plugin_suites, gates = plugin_plan(plugin)
        suites = args.suite or plugin_suites
        if args.socket:
            system = socket_system(args.socket, plugin["id"])
    elif args.socket:
        raise SystemExit("--socket needs --plugin to name the model")
    report = evaluate(system or load_system(system_spec), suites, args.repeats, gates)
    print(format_report(report))
    if args.json_out:
        with open(args.json_out, "w", encoding="utf-8") as handle:
            json.dump(to_json(report), handle, indent=2, ensure_ascii=False)
    if report.failed or (args.strict and report.pending):
        sys.exit(1)
