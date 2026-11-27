"""``myna-bench text --eval``: per-operation accuracy of text-processing arms.

Grades any text-in, text-out arm on the evaluation corpora of ``text_corpus``
(``eval/run1``, the corpus the model arms were developed against,
``eval/run2``, written blind, and ``eval/run3``, the blind tidy slice). An *item*
is an input and the outputs that count as right; its ``kind`` is ``positive``
(must change), ``negative`` (must stay) or ``clean`` (an ordinary sentence, any
change is a break).

Per (operation, language, arm):

``acc``     share of corpus items (positives and negatives) whose output equals
            a reference after normalisation.
``pos``     accuracy on positives; ``keep`` accuracy on negatives.
``break``   share of clean sentences the arm changed (lower is better).
``damage``  per 1000 items: wrong and not recoverable, meaning the output's
            letters are not a subsequence of the input's (an invented word) or
            a tidy kept under half the letters.
``p50/p95`` per-call latency in milliseconds, as the arm ran here.
``dAcc``    acc minus the baseline arm's, with a paired bootstrap 95% interval.

A hybrid arm ``hyb:<arm>`` takes the arm's output when a verifier passes and
the baseline's otherwise, which is how a model sits behind a conservative edit.

Arms are named by a spec: ``identity``, ``tagger:<dir>[:tau]``
(the disfluency tagger directly, bare: no rule before it), ``socket:<path>:<model>`` (a running
``myna-text`` daemon) and ``openai:<base-url>#<model>[#fewshot|task]`` (any
OpenAI-compatible chat endpoint such as ``llama-server``). ``name=spec``
renames an arm; the arm called ``rules`` is the baseline.
"""

from __future__ import annotations

import argparse
import json
import random
import time
import unicodedata
import urllib.request
from collections import defaultdict
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path

from myna.benchmarker import text_corpus, text_eval_prompts

BASELINE = "rules"
HYBRID = "hyb:"
BOOTSTRAP_RESAMPLES = 2000
OPS = ("tidy", "numbers", "addresses", "commands", "casing", "emoji")

_SPACES = {"\u00a0": " ", "\u202f": " ", "\u2009": " ", "\u2007": " "}
_QUOTES = {"\u2019": "'", "\u2018": "'", "\u201c": '"', "\u201d": '"', "\u00ab": '"', "\u00bb": '"'}


def norm(text: str) -> str:
    """The comparison form: NFC, one kind of space and quote, no emoji variation selector."""
    text = unicodedata.normalize("NFC", text)
    for a, b in {**_SPACES, **_QUOTES}.items():
        text = text.replace(a, b)
    text = text.replace("\ufe0f", "")
    return "\n".join(" ".join(line.split()) for line in text.strip().split("\n"))


def letters(text: str) -> str:
    folded = unicodedata.normalize("NFD", text.casefold())
    return "".join(c for c in folded if c.isalpha())


def subsequence(small: str, big: str) -> bool:
    rest = iter(big)
    return all(c in rest for c in small)


def correct(item: text_corpus.EvalItem, output: str) -> bool:
    return norm(output) in {norm(r) for r in item.refs}


def unrecoverable(item: text_corpus.EvalItem, output: str) -> bool:
    out, inp = letters(output), letters(item.input)
    if not subsequence(out, inp):
        return True
    return item.op == "tidy" and len(out) < 0.5 * len(inp)


def damaged(item: text_corpus.EvalItem, output: str) -> bool:
    return not correct(item, output) and unrecoverable(item, output)


def verified(item: text_corpus.EvalItem, output: str) -> bool:
    """May a hybrid take this output instead of the baseline's?"""
    if not output.strip() or not subsequence(letters(output), letters(item.input)):
        return False
    if item.op != "commands" and output.count("\n") > item.input.count("\n"):
        return False
    return not (item.op == "tidy" and len(letters(output)) < 0.5 * len(letters(item.input)))


def quantile(values: Sequence[float], q: float) -> float:
    """The value at index ``int(q * n)`` of the sorted values; NaN when empty."""
    if not values:
        return float("nan")
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(q * len(ordered)))]


def bootstrap(a: Sequence[int], b: Sequence[int]) -> tuple[float, float]:
    """95% interval of the mean paired difference ``a - b`` (seeded, so reproducible)."""
    rng = random.Random(1)
    size = len(a)
    diffs = sorted(
        sum(a[i] - b[i] for i in (rng.randrange(size) for _ in range(size))) / size
        for _ in range(BOOTSTRAP_RESAMPLES)
    )
    return diffs[int(0.025 * BOOTSTRAP_RESAMPLES)], diffs[int(0.975 * BOOTSTRAP_RESAMPLES)]


@dataclass(frozen=True)
class Result:
    output: str
    ms: float


Results = dict[str, dict[str, Result]]  # arm -> item id -> result


@dataclass(frozen=True)
class Row:
    acc: float
    pos: float
    keep: float
    brk: float
    damage: float
    p50: float
    p95: float
    delta: tuple[float, float, float] | None  # dAcc, low, high; None for the baseline


def _mean(values: Sequence[int]) -> float:
    return 100 * sum(values) / len(values) if values else float("nan")


def score_group(
    items: Sequence[text_corpus.EvalItem],
    arm: Mapping[str, Result],
    baseline: Mapping[str, Result] | None,
) -> Row:
    corpus = [i for i in items if i.kind != "clean"]
    clean = [i for i in items if i.kind == "clean"]
    hit = [int(correct(i, arm[i.id].output)) for i in corpus]
    brk = [int(norm(arm[i.id].output) != norm(i.input)) for i in clean]
    dmg = sum(damaged(i, arm[i.id].output) for i in items)
    millis = [arm[i.id].ms for i in items]
    delta = None
    if baseline is not None and corpus:
        base = [int(correct(i, baseline[i.id].output)) for i in corpus]
        low, high = bootstrap(hit, base)
        delta = (100 * (sum(hit) - sum(base)) / len(hit), 100 * low, 100 * high)
    return Row(
        acc=_mean(hit),
        pos=_mean([h for h, i in zip(hit, corpus, strict=True) if i.kind == "positive"]),
        keep=_mean([h for h, i in zip(hit, corpus, strict=True) if i.kind == "negative"]),
        brk=_mean(brk),
        damage=1000 * dmg / len(items),
        p50=quantile(millis, 0.5),
        p95=quantile(millis, 0.95),
        delta=delta,
    )


def with_hybrids(
    items: Sequence[text_corpus.EvalItem], results: Results, baseline: str, models: Sequence[str]
) -> Results:
    """``results`` plus ``hyb:<m>``: the model's verified output, else the baseline's."""
    out = dict(results)
    base = results.get(baseline)
    if base is None:
        return out
    for name in models:
        arm = results.get(name)
        if arm is None or name == baseline:
            continue
        mixed: dict[str, Result] = {}
        for item in items:
            got = arm.get(item.id)
            if got is None or item.id not in base:
                continue
            ok = verified(item, got.output)
            mixed[item.id] = Result(
                got.output if ok else base[item.id].output, got.ms + base[item.id].ms
            )
        out[HYBRID + name] = mixed
    return out


def score(
    items: Sequence[text_corpus.EvalItem], results: Results, baseline: str = BASELINE
) -> dict[tuple[str, str], dict[str, Row]]:
    """Rows per (op, lang) and arm, for the arms that answered every item of the group."""
    groups: dict[tuple[str, str], list[text_corpus.EvalItem]] = defaultdict(list)
    for item in items:
        groups[(item.op, item.lang)].append(item)
    base = results.get(baseline)
    table: dict[tuple[str, str], dict[str, Row]] = {}
    for key, group in sorted(groups.items()):
        rows: dict[str, Row] = {}
        for name, arm in results.items():
            if all(i.id in arm for i in group) and (
                base is None or all(i.id in base for i in group)
            ):
                rows[name] = score_group(group, arm, None if name == baseline else base)
        if rows:
            table[key] = rows
    return table


def _order(names: Sequence[str], baseline: str) -> list[str]:
    plain = sorted(n for n in names if not n.startswith(HYBRID) and n != baseline)
    hybrids = sorted(n for n in names if n.startswith(HYBRID))
    return [*([baseline] if baseline in names else []), *plain, *hybrids]


def format_markdown(
    items: Sequence[text_corpus.EvalItem],
    table: Mapping[tuple[str, str], Mapping[str, Row]],
    title: str = "Text processing evaluation",
) -> str:
    sizes: dict[tuple[str, str], tuple[int, int]] = {}
    for key in table:
        group = [i for i in items if (i.op, i.lang) == key]
        sizes[key] = (sum(i.kind != "clean" for i in group), sum(i.kind == "clean" for i in group))
    lines = [
        f"# {title}",
        "",
        "acc/pos/keep/break in %, damage per 1000 items, latency in ms.",
        "",
    ]
    for (op, lang), rows in table.items():
        corpus, clean = sizes[(op, lang)]
        lines += [f"## {op} {lang}  (corpus {corpus}, clean {clean})", ""]
        lines += [
            "| arm | acc | dAcc vs baseline [95% CI] | pos | keep | break | damage | p50 | p95 |",
            "|---|---|---|---|---|---|---|---|---|",
        ]
        for name in _order(list(rows), BASELINE if BASELINE in rows else next(iter(rows))):
            r = rows[name]
            ci = "-"
            if r.delta is not None:
                ci = f"{r.delta[0]:+.1f} [{r.delta[1]:+.1f}, {r.delta[2]:+.1f}]"
            lines.append(
                f"| {name} | {r.acc:.1f} | {ci} | {r.pos:.1f} | {r.keep:.1f} | {r.brk:.1f} "
                f"| {r.damage:.1f} | {r.p50:.0f} | {r.p95:.0f} |"
            )
        lines.append("")
    return "\n".join(lines)


# -- arms ------------------------------------------------------------------


@dataclass
class Arm:
    """A text-in, text-out system with the operations and languages it can be asked."""

    name: str
    run: Callable[[text_corpus.EvalItem], str]
    ops: frozenset[str] = frozenset(OPS)
    langs: frozenset[str] = frozenset({"en", "fr", "de", "es"})
    model: bool = False

    def covers(self, item: text_corpus.EvalItem) -> bool:
        return item.op in self.ops and item.lang in self.langs


def tidy_arm(name: str, run: Callable[[str], str]) -> Arm:
    """A cleanup model: it reads tidy items, in every language the corpus has."""
    return Arm(name, lambda item: run(item.input), frozenset({"tidy"}))


def chat_request(
    url: str, model: str, prompt: str, item: text_corpus.EvalItem, timeout: float = 300.0
) -> str:
    """One greedy chat completion for ``item``; the reply with any think block removed."""
    messages = text_eval_prompts.messages(
        prompt, item.op, item.lang, item.input, "qwen3" in model.lower()
    )
    body = {
        "model": model,
        "messages": messages,
        "temperature": 0,
        "seed": 1,
        "cache_prompt": True,
        "max_tokens": 64 + 6 * len(item.input.split()),
    }
    request = urllib.request.Request(
        url.rstrip("/") + "/v1/chat/completions",
        json.dumps(body).encode(),
        {"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        content = json.load(response)["choices"][0]["message"]["content"] or ""
    return content.split("</think>", 1)[-1].strip()


def parse_arm(spec: str) -> Arm:
    """An arm from a spec (see the module docstring)."""
    name, sep, rest = spec.partition("=")
    if not sep or ":" in name or "#" in name or "/" in name:
        name, rest = "", spec
    kind, _, arg = rest.partition(":")
    arm: Arm
    if kind == "identity":
        arm = Arm("identity", lambda item: item.input)
    elif kind == "tagger":
        from myna.text.tagger import Tagger

        path, _, tau = arg.partition(":")
        arm = tidy_arm("tagger", Tagger.load(Path(path), float(tau or 0.9)).edit)
    elif kind == "socket":
        from myna.benchmarker.text_gate import socket_system

        path, _, model = arg.rpartition(":")
        tidy = socket_system(path, model).tidy
        if tidy is None:
            raise SystemExit(f"{model} on {path} is not a cleanup model")
        arm = tidy_arm(f"socket:{model}", tidy)
    elif kind == "openai":
        url, _, tail = arg.partition("#")
        model, _, prompt = tail.partition("#")
        prompt = prompt or text_eval_prompts.FEWSHOT
        if not url or not model or prompt not in text_eval_prompts.FORMATS:
            formats = "|".join(text_eval_prompts.FORMATS)
            raise SystemExit(f"openai arm wants <base-url>#<model>[#{formats}]")
        ops = frozenset(op for op in OPS if text_eval_prompts.supported(prompt, op, "en"))
        arm = Arm(model, lambda item: chat_request(url, model, prompt, item), ops, model=True)
    else:
        raise SystemExit(f"unknown arm {spec!r}: identity, tagger, socket, openai")
    if name:
        arm.name = name
    return arm


def run_arm(arm: Arm, items: Sequence[text_corpus.EvalItem]) -> dict[str, Result]:
    """Run ``arm`` on the items it covers."""
    results: dict[str, Result] = {}
    for item in items:
        if arm.covers(item):
            started = time.perf_counter()
            output = arm.run(item)
            results[item.id] = Result(output, (time.perf_counter() - started) * 1000)
    return results


def read_results(directory: Path) -> Results:
    """Recorded outputs: every ``<arm>.jsonl`` of a directory (a ``model-`` prefix is dropped)."""
    results: Results = {}
    for path in sorted(directory.glob("*.jsonl")):
        if path.name != "items.jsonl":
            rows = (json.loads(x) for x in path.read_text(encoding="utf-8").splitlines() if x)
            results[path.stem.removeprefix("model-")] = {
                r["id"]: Result(r["output"], float(r["ms"])) for r in rows
            }
    return results


def add_arguments(parser: argparse.ArgumentParser) -> None:
    group = parser.add_argument_group("per-operation evaluation (--eval)")
    group.add_argument(
        "--eval",
        dest="eval_run",
        choices=text_corpus.EVAL_RUNS,
        help="grade arms per operation on this corpus instead of the ship gates "
        "(run1: developed against, run2: blind, run3: blind tidy only)",
    )
    group.add_argument(
        "--arm",
        action="append",
        default=[],
        help="an arm spec (repeatable): identity, tagger:<dir>[:tau], "
        "socket:<path>:<model>, openai:<url>#<model>[#fewshot|task]; name=spec renames",
    )
    group.add_argument("--op", action="append", help="limit to this operation (repeatable)")
    group.add_argument("--lang", action="append", help="limit to this language (repeatable)")
    group.add_argument(
        "--results",
        help="directory of recorded <arm>.jsonl outputs ({id, output, ms}) to score too",
    )
    group.add_argument("--report", help="write the markdown report here")


def cmd_eval(args: argparse.Namespace) -> None:
    run = args.eval_run
    skip = text_eval_prompts.shot_inputs() if run == "run1" else frozenset()
    items = [
        i
        for i in text_corpus.load_eval(run, skip_inputs=skip)
        if (not args.op or i.op in args.op) and (not args.lang or i.lang in args.lang)
    ]
    if not items:
        raise SystemExit("no item matches --op/--lang")
    results = read_results(Path(args.results)) if args.results else {}
    models = []
    for spec in args.arm:
        arm = parse_arm(spec)
        results[arm.name] = run_arm(arm, items)
        if arm.model:
            models.append(arm.name)
    if not results:
        raise SystemExit("nothing to score: give --arm or --results")
    scored = score(items, with_hybrids(items, results, BASELINE, models))
    report = format_markdown(items, scored, f"Text processing evaluation ({run})")
    print(report)
    if args.report:
        Path(args.report).write_text(report + "\n", encoding="utf-8")
