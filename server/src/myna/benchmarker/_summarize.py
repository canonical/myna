"""Aggregate bench records into a cross-label comparison table.

    myna-bench summarize --in results.jsonl --by-category

Reads the JSONL written by the sweep runner and produces the model x hardware
comparison the specs need: one row per label (e.g. ``myna-whisper/cpu/tiny/batch``),
with micro-averaged WER/CER (total edits / total reference, so long clips count
proportionally), RTFx and finalize-latency percentiles, then 95% bootstrap
intervals over clips (``--no-ci`` skips them). ``compare A B`` is the paired
version for two rows on the same clips.

Records are deduplicated by (label, clip, repeat, phase), keeping the most
recent - so re-running a label replaces its old rows rather than
double-counting. Warmup rows are read past: they record what ran, not a score.
"""

from __future__ import annotations

import argparse
import importlib
import json
from pathlib import Path
from typing import Any, TypedDict

import yaml

from myna.benchmarker._pace import REALTIME
from myna.benchmarker._schedule import COLD, MEASURED, WARMUP
from myna.benchmarker._stats import (
    ClipSample,
    latency_percentile,
    percentile,
    repeat_cv,
    rtfx,
    sample_floor,
)
from myna.testbed.metrics import NORMALIZER_VERSION, SECONDARY_NORMALIZER_VERSION

Record = dict[str, Any]

# Version of the results schema, stamped on the machine header and every row.
# Readers treat a missing field as unknown, never as an error, so a file from
# before a bump still summarizes and merges. 1 is the unstamped schema; 2 adds
# the environment manifest (os, gpus, harness, installed artifacts, served
# runtime); 3 adds the telemetry trace to ``*-resources.jsonl`` (rows of kind
# ``sample``) and energy, temperature and throttling to its per-cell row.
SCHEMA_VERSION = 3

# A row's identity in a results file. The machine is half of it: a leaderboard
# holds the same <snap>/<engine>/<model>/<mode> measured on many machines, and
# that is the whole point of collecting them. Keyed on label alone, merging two
# submissions kept one and silently dropped the other.
RowKey = tuple[str, str]


class SummaryRow(TypedDict):
    clips: int
    label: str
    machine: str
    wer: float | None
    cer: float | None
    wer_whisper: float | None
    cer_whisper: float | None
    rtf: float | None
    rtfx: float | None
    median_final: float | None
    p95_final: float | None
    p99_final: float | None
    timed_clips: int
    repeat_cv: float | None
    cold_ready: float | None
    warm_ready: float | None
    audio: float
    peak_rss_mb: float | None
    peak_vram_mb: float | None
    j_per_audio_s: float | None
    throttled: dict[str, bool | None] | None
    starved: int


def machine_of(record: Record) -> str:
    """Which machine produced a row. ``unknown`` for rows written before
    provenance was stamped, so they group together instead of vanishing."""
    provenance = record.get("provenance")
    if isinstance(provenance, dict) and provenance.get("machine"):
        return str(provenance["machine"])
    return record.get("machine") or "unknown"


def row_key(record: Record) -> RowKey:
    return machine_of(record), record["label"]


def _load_latest(
    path: Path, *, keep_errors: bool = False
) -> tuple[list[Record], dict[RowKey, tuple[str, str]]]:
    """Return (clip records, {(machine, label): (status, reason)}), last wins.

    ``keep_errors`` also returns rows whose backend errored, for a reader that
    reports them as failed requests. An error never replaces a success: each
    key keeps its last successful row, which every aggregate scores, and its
    last error only when no success came after it.

    Cold samples and each repeat are keyed separately, so a clip measured cold,
    warm and in several passes keeps every row. Warmup rows are dropped here,
    which keeps them out of every table. A row written before repeats existed
    reads as repeat 0 in the phase its ``cold`` flag names.

    Status records (``{"machine", "label", "status", "reason"}``, no "clip")
    come from the sweep runner: "usability_fail" when a target ran out of its
    wall-clock budget mid-sweep, "broken" when it crashed outright, "ok" on a
    clean completion. A run that didn't finish leaves fewer clip records for
    that label - indistinguishable, from clip records alone, from "this
    category wasn't scheduled". The status record is the one durable signal
    that a label's partial data means it *failed*, not that it scored a clean
    0%; last-occurrence-wins so a later clean rerun clears an earlier failure.
    """
    if not path.exists():
        raise SystemExit(f"no results at {path}")
    latest: dict[tuple[str, str, str, int, str], Record] = {}
    errors: dict[tuple[str, str, str, int, str], Record] = {}
    statuses: dict[RowKey, tuple[str, str]] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if not raw:
            continue
        rec = json.loads(raw)
        if rec.get("type") == "machine":
            continue
        if "status" in rec and "clip" not in rec:
            statuses[row_key(rec)] = (rec["status"], rec.get("reason", ""))
            continue
        phase = rec.get("phase") or (COLD if rec.get("cold", False) else MEASURED)
        if phase == WARMUP:
            continue
        machine, label = row_key(rec)
        key = (machine, label, rec["clip"], int(rec.get("repeat") or 0), phase)
        if rec.get("error"):
            # The backend errored instead of transcribing (missing runtime
            # library, unloadable weights). Its empty hypothesis would score as
            # a flawless 100% WER and drag the label's micro-average with it.
            if keep_errors:
                errors[key] = rec
            continue
        errors.pop(key, None)
        latest[key] = rec
    return [*latest.values(), *errors.values()], statuses


def one_machine_per_name(records: list[Record]) -> None:
    """Refuse a file where one machine name covers two different CPUs.

    Hostnames are not unique across a team, and rows are keyed by name - two
    laptops both called "framework" would merge into one row and lose a
    submission, which is exactly the bug this key was widened to fix. Catch it
    rather than average across two machines.
    """
    cpus: dict[str, set[str]] = {}
    for record in records:
        provenance = record.get("provenance")
        cpu = provenance.get("cpu") if isinstance(provenance, dict) else None
        if cpu:
            cpus.setdefault(machine_of(record), set()).add(str(cpu))
    clashing = {name: sorted(seen) for name, seen in cpus.items() if len(seen) > 1}
    if clashing:
        detail = "; ".join(f"{name}: {seen}" for name, seen in sorted(clashing.items()))
        raise SystemExit(
            f"one machine name covers more than one CPU ({detail}) - two hosts share a "
            "hostname, so their rows would merge and one submission would be lost. "
            "Rename one host, or split the file."
        )


def one_corpus(records: list[Record], wanted: str | None) -> tuple[list[Record], str]:
    """Records for a single corpus. Two corpora in one file is not a table to
    be qualified, it is a comparison that cannot be made."""
    ids = {r.get("corpus_id") for r in records}
    if not ids:
        raise SystemExit("no clip records to aggregate")
    if None in ids:
        raise SystemExit(
            "records with no corpus_id: they were measured before the corpus was "
            "stamped, and nothing says what audio produced them - drop them"
        )
    known = {i for i in ids if i is not None}
    if wanted is None:
        if len(known) > 1:
            raise SystemExit(
                "records span " + ", ".join(sorted(known)) + " - a WER micro-averaged "
                "across two corpora compares nothing; re-run on one, or pass --corpus"
            )
        wanted = known.pop()
    return [r for r in records if r["corpus_id"] == wanted], wanted


def one_normalizer_version(records: list[Record]) -> None:
    """Refuse a file whose rows were scored under different ``normalize()``
    behavior. A WER micro-averaged across normalizer versions blends two
    different scoring rules into one number that describes neither."""
    versions = {r.get("normalizer_version") for r in records}
    if len(versions) > 1:
        detail = ", ".join(str(v) for v in sorted(versions, key=lambda v: (v is None, v)))
        raise SystemExit(
            f"records span normalizer versions ({detail}) - rescore everything with the "
            "current myna.testbed.metrics normalizer before comparing, or split the file"
        )
    # Rows from before the secondary score have none; _whisper_rate leaves
    # such a cell blank rather than refusing the primary table over it.
    secondary = {
        r["secondary_normalizer_version"] for r in records if r.get("secondary_normalizer_version")
    }
    if len(secondary) > 1:
        raise SystemExit(
            f"records span secondary normalizer versions ({', '.join(sorted(secondary))}) - "
            "rescore with the current vendored Whisper normalizer, or split the file"
        )


def _whisper_rate(records: list[Record], edits: str, total: str) -> float | None:
    """A cell's micro-averaged rate under Whisper's normaliser, or None when
    any row lacks it: a rate over a subset of the clips is not the cell's."""
    if any(r.get(total) is None for r in records):
        return None
    reference = sum(r[total] for r in records)
    return sum(r[edits] for r in records) / reference if reference else None


def _latency(record: Record) -> float | None:
    """A warm row's finalize latency, if it measured one a user would see.

    A realtime feed that fell behind delivered a burst no microphone would.
    """
    if record.get("pace_starved"):
        return None
    return record.get("finalize_latency")


def _throughput(record: Record) -> tuple[float, float] | None:
    """(audio, processing) seconds of a row that measured throughput.

    A realtime feed's decode time is set by the pace, not the model.
    """
    if record.get("rtf") is None or record.get("pace") == REALTIME:
        return None
    return record["audio_seconds"], record["rtf"] * record["audio_seconds"]


def clip_samples(records: list[Record]) -> dict[str, ClipSample]:
    """One cell's warm rows folded per clip, every repeat into its clip."""
    folded: dict[str, ClipSample] = {}
    for r in records:
        if r.get("cold"):
            continue
        prior = folded.get(r["clip"], ClipSample())
        latency = _latency(r)
        timed = _throughput(r) or (0.0, 0.0)
        scored = r.get("ref_words_whisper_norm") is not None
        w_edits, w_words, c_edits, c_chars = (
            (
                r["wer_whisper_norm_edits"],
                r["ref_words_whisper_norm"],
                r["cer_whisper_norm_edits"],
                r["ref_chars_whisper_norm"],
            )
            if scored
            else (0, 0, 0, 0)
        )
        folded[r["clip"]] = ClipSample(
            wer_edits=prior.wer_edits + r["wer_edits"],
            ref_words=prior.ref_words + r["ref_words"],
            cer_edits=prior.cer_edits + r["cer_edits"],
            ref_chars=prior.ref_chars + r["ref_chars"],
            audio_seconds=prior.audio_seconds + timed[0],
            processing_seconds=prior.processing_seconds + timed[1],
            latencies=prior.latencies + ((latency,) if latency is not None else ()),
            wer_whisper_edits=prior.wer_whisper_edits + w_edits,
            ref_words_whisper=prior.ref_words_whisper + w_words,
            cer_whisper_edits=prior.cer_whisper_edits + c_edits,
            ref_chars_whisper=prior.ref_chars_whisper + c_chars,
            whisper_scored=prior.whisper_scored and scored,
        )
    return folded


def _groups(records: list[Record]) -> dict[RowKey, list[Record]]:
    groups: dict[RowKey, list[Record]] = {}
    for rec in records:
        groups.setdefault(row_key(rec), []).append(rec)
    return groups


def _summarize(records: list[Record]) -> dict[RowKey, SummaryRow]:
    """Group records by (machine, label) and micro-average the metrics.

    Accuracy and warm latency come from the warm rows; cold-load latency is
    reported separately from the cold samples (``--cold`` bench runs).
    """
    summary: dict[RowKey, SummaryRow] = {}
    for key, recs in _groups(records).items():
        machine, label = key
        warm = [r for r in recs if not r.get("cold", False)]
        cold = [r for r in recs if r.get("cold", False)]
        # A realtime feed that fell behind measured no live latency; its
        # transcript still scores.
        starved = [r for r in warm if r.get("pace_starved")]
        clips = list(clip_samples(warm).values())
        # Pure model-load wait (session open -> ready), independent of decode.
        cold_readys = [r["time_to_ready"] for r in cold if r.get("time_to_ready") is not None]
        warm_readys = [r["time_to_ready"] for r in warm if r.get("time_to_ready") is not None]
        rtfs = [r["rtf"] for r in warm if _throughput(r) is not None]
        wer_edits = sum(r["wer_edits"] for r in warm)
        ref_words = sum(r["ref_words"] for r in warm)
        cer_edits = sum(r["cer_edits"] for r in warm)
        ref_chars = sum(r["ref_chars"] for r in warm)
        summary[key] = {
            "clips": len(warm),
            "label": label,
            "machine": machine,
            # None, never 0.0: a label whose warm rows all errored - or which
            # only ever ran as a --cold sample - has no score, and a 0.00%
            # would both print as flawless and rank first.
            "wer": wer_edits / ref_words if ref_words else None,
            "cer": cer_edits / ref_chars if ref_chars else None,
            "wer_whisper": _whisper_rate(warm, "wer_whisper_norm_edits", "ref_words_whisper_norm"),
            "cer_whisper": _whisper_rate(warm, "cer_whisper_norm_edits", "ref_chars_whisper_norm"),
            "rtf": percentile(rtfs, 0.5),
            "rtfx": rtfx(clips),
            "median_final": latency_percentile(clips, 0.5),
            # None below the sample floor; timed_clips says why.
            "p95_final": latency_percentile(clips, 0.95),
            "p99_final": latency_percentile(clips, 0.99),
            "timed_clips": sum(1 for c in clips if c.latencies),
            "repeat_cv": repeat_cv([c.latencies for c in clips]),
            # cold-load = model residency wait only (time_to_ready), from --cold
            # samples; the warm reload should be ~0.
            "cold_ready": max(cold_readys) if cold_readys else None,
            "warm_ready": percentile(warm_readys, 0.5),
            "audio": sum(r["audio_seconds"] for r in warm),
            "peak_rss_mb": None,
            "peak_vram_mb": None,
            "j_per_audio_s": None,
            "throttled": None,
            "starved": len(starved),
        }
    return summary


def _load_resources(path: Path) -> dict[RowKey, Record]:
    """Read the sweep's per-cell resource rows, keyed like every other row.

    The 1 Hz telemetry trace (``kind: sample``) is skipped. Last occurrence
    wins. Absent file -> empty (resource columns are hidden).
    """
    if not path.exists():
        return {}
    peaks: dict[RowKey, Record] = {}
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw:
            rec = json.loads(raw)
            if rec.get("kind") != "sample":
                peaks[row_key(rec)] = rec
    return peaks


def resources_path_for(out: Path) -> Path:
    """Sidecar path for a results file. One rule, so writer and reader agree."""
    return out.parent / (out.stem + "-resources.jsonl")


def _f(x: object, spec: str = "6.2f") -> str:
    return format(x, spec) if isinstance(x, (int, float)) else "    --"


TOO_FEW = "n too small"


def _too_few(samples: int, q: float) -> bool:
    """Whether a ``q`` percentile is missing for want of samples, not of data."""
    return 0 < samples < sample_floor(q)


def _tail(value: float | None, samples: int, q: float, width: int) -> str:
    """A tail latency cell: the value, or why there is none."""
    return f"{TOO_FEW:>{width}}" if _too_few(samples, q) else _f(value, f"{width}.3f")


def _rtfx(value: object) -> str:
    return f"{value:.1f}" if isinstance(value, (int, float)) else "--"


def _scaled(rate: object) -> float | None:
    """An error rate as a percentage, keeping "not scored" distinct from zero."""
    return rate * 100 if isinstance(rate, (int, float)) else None


def _throttle(throttled: object) -> str:
    """Which side throttled during the cell; -- where nothing could tell."""
    if not isinstance(throttled, dict) or all(v is None for v in throttled.values()):
        return "--"
    hit = [side for side in ("cpu", "gpu") if throttled.get(side)]
    return "+".join(hit) if hit else "no"


def _speed(rtf: object) -> str:
    """Format RTF as a human-readable speed multiplier.

    0.018 -> '55x', 0.046 -> '22x', 1.682 -> '0.6x'.  Values >= 10x are
    shown as integers; below that one decimal keeps enough resolution to
    distinguish e.g. 5.2x from 4.8x.
    """
    if not isinstance(rtf, (int, float)) or rtf <= 0:
        return "   --"
    x = 1.0 / rtf
    return f"{x:.0f}x" if x >= 10 else f"{x:.1f}x"


# Ranking field per --sort choice. All of these are "lower is better" metrics
# (error rate, RTF, latency), so ascending sort puts the best performer first
# uniformly - no special-casing a "higher is better" field.
RANK_FIELDS = {
    "wer": "wer",
    "cer": "cer",
    "speed": "rtf",
    "latency": "median_final",
    "cold-load": "cold_ready",
}


def ranked_labels(
    summary: dict[RowKey, SummaryRow], sort: str, statuses: dict[RowKey, tuple[str, str]]
) -> list[RowKey]:
    """Rows ordered best-first by ``sort``; missing values sort last.

    A row whose last recorded status is not "ok" (usability_fail or broken)
    sinks below every clean completion regardless of metric value - its
    WER/speed was measured on however many clips it got through before failing,
    not the full sweep, so it is not a comparable data point and must never
    rank as if it beat a target that actually finished.
    """

    def failed(key: RowKey) -> bool:
        return statuses.get(key, ("ok", ""))[0] != "ok"

    if sort == "label":
        return sorted(summary, key=lambda key: (failed(key), key[1], key[0]))
    field = RANK_FIELDS[sort]
    return sorted(
        summary,
        key=lambda key: (
            failed(key),
            summary[key].get(field) is None,
            summary[key].get(field) or 0.0,
            key,
        ),
    )


def _print_overall(
    summary: dict[RowKey, SummaryRow],
    order: list[RowKey],
    statuses: dict[RowKey, tuple[str, str]],
) -> None:
    # The machine column is unconditional once a file holds more than one: on a
    # leaderboard the machine is half of what a row means, and a table that
    # hides it invites reading two hosts' numbers as a single ranking.
    machines = {key[0] for key in summary}
    show_machine = len(machines) > 1 or any(m != "unknown" for m in machines)
    show_res = any(s.get("peak_rss_mb") for s in summary.values())
    # Sized to the data, not to a guess: labels grew from "cpu/small" to
    # "whisper/cpu/base/streaming" when the runner started reporting the engine,
    # model and mode it actually measured, and a fixed 20 sheared every column
    # to the right of it.
    lw = max([len("label"), *(len(key[1]) for key in summary)])
    mw = max([len("machine"), *(len(key[0]) for key in summary)]) if show_machine else 0
    mh = f"{'machine':{mw}} " if show_machine else ""
    show_energy = any(s.get("throttled") is not None for s in summary.values())
    rh = f"{'RSS MB':>9} {'VRAM MB':>9}" if show_res else ""
    rh += f" {'J/aud s':>8} {'throttle':>8}" if show_energy else ""
    header = (
        f"{'#':>3} {'label':{lw}} {'status':>13} {mh}{'clips':>5} "
        f"{'WER%':>7} {'CER%':>7} {'WERw%':>7} {'CERw%':>7} {'speed':>6} {'RTFx':>7} "
        f"{'med final':>11} {'p95 final':>11} {'p99 final':>11} {'rep CV%':>7} "
        f"{'cold load':>10} {rh}"
    )
    print(header)
    print("-" * len(header.rstrip()))
    for rank, key in enumerate(order, start=1):
        s = summary[key]
        status, reason = statuses.get(key, ("--", ""))
        status_col = status.upper() if status != "--" else "--"
        if reason:
            status_col = f"{status_col} ({reason[:20]})"
        mc = f"{key[0]:{mw}} " if show_machine else ""
        rc = (
            f"{_f(s.get('peak_rss_mb'), '9.1f')} {_f(s.get('peak_vram_mb'), '9.1f')}"
            if show_res
            else ""
        )
        if show_energy:
            rc += f" {_f(s.get('j_per_audio_s'), '8.3f')} {_throttle(s.get('throttled')):>8}"
        print(
            f"{rank:>3} {key[1]:{lw}} {status_col:>13} {mc}{s['clips']:>5} "
            f"{_f(_scaled(s['wer']), '7.2f')} {_f(_scaled(s['cer']), '7.2f')} "
            f"{_f(_scaled(s['wer_whisper']), '7.2f')} {_f(_scaled(s['cer_whisper']), '7.2f')} "
            f"{_speed(s['rtf']):>6} {_rtfx(s['rtfx']):>7} "
            f"{_f(s['median_final'], '11.3f')} "
            f"{_tail(s['p95_final'], s['timed_clips'], 0.95, 11)} "
            f"{_tail(s['p99_final'], s['timed_clips'], 0.99, 11)} "
            f"{_f(_scaled(s['repeat_cv']), '7.1f')} "
            f"{_f(s['cold_ready'], '10.3f')} {rc}"
        )
    print(
        "\nmed/p95/p99 final are seconds (end-of-audio -> committed text); a p95 needs"
        " 60 timed clips and a p99 300 (repeats pool, but do not count), or it reads 'n too small'."
    )
    print(
        f"WER/CER use Myna's normaliser (v{NORMALIZER_VERSION}, primary); WERw/CERw use Whisper's"
        f" ({SECONDARY_NORMALIZER_VERSION}: EnglishTextNormalizer for English,"
        " BasicTextNormalizer otherwise), as the Open ASR Leaderboard does; -- = not scored."
    )
    print(
        "speed = 1 / median per-clip RTF; RTFx = total audio / total processing"
        " (Open ASR Leaderboard), batch size 1; higher is faster for both."
    )
    print("rep CV% = median within-clip spread of finalize latency across repeats (noise).")
    print("cold load = model residency wait (session open -> ready), from --cold runs.")
    print(
        "status: OK = clean full sweep; USABILITY_FAIL = ran out of budget mid-sweep (metrics"
        " are partial, not comparable); BROKEN = crashed; -- = no status record for this row."
        " Failed/broken rows always sort last regardless of --sort."
    )
    if show_res:
        print("RSS/VRAM = peak memory during the run.")
    if show_energy:
        print(
            "J/aud s = RAPL package + every NVIDIA GPU's energy over the whole cell (cold,"
            " warmup, measured) per second of audio fed, -- when any of them went unread;"
            " throttle = cpu/gpu slowed by power or heat during the cell, -- = not observable"
            " here."
        )
    starved = sum(s["starved"] for s in summary.values())
    if starved:
        print(
            f"{starved} starved realtime clip(s): the feed fell over a chunk behind real time,"
            " so their finalize latency is left out of med/p95 final."
        )


def _print_by_category(
    records: list[Record], order: list[RowKey], statuses: dict[RowKey, tuple[str, str]]
) -> None:
    records = [r for r in records if not r.get("cold", False)]  # warm only
    present = {row_key(r) for r in records}
    keys = [key for key in order if key in present]
    cats = sorted({r["category"] for r in records})
    # micro WER per (row, category)
    cell: dict[tuple[RowKey, str], tuple[int, int]] = {}
    for r in records:
        index = (row_key(r), r["category"])
        e, w = cell.get(index, (0, 0))
        cell[index] = (e + r["wer_edits"], w + r["ref_words"])

    show_machine = len({key[0] for key in keys}) > 1
    names = [f"{key[1]} @ {key[0]}" if show_machine else key[1] for key in keys]
    # Rows on Y, categories on X. Column width from the widest category name.
    lw = max([len("label"), *(len(n) for n in names)])
    cw = max(6, *(len(c) for c in cats)) if cats else 6
    print("\nWER% by category ('--' = no clips scored in that category, not a 0% pass)")
    header = f"{'label':{lw}} " + " ".join(f"{cat:>{cw}}" for cat in cats)
    print(header)
    print("-" * len(header))
    for key, name in zip(keys, names, strict=True):
        status, _ = statuses.get(key, ("--", ""))
        cells = []
        for cat in cats:
            e, w = cell.get((key, cat), (0, 0))
            cells.append(f"{'--':>{cw}}" if not w else f"{e / w * 100:>{cw}.1f}")
        marker = f" [{status.upper()}]" if status not in ("ok", "--") else ""
        print(f"{name:{lw}} " + " ".join(cells) + marker)


# Languages written without spaces between words: a "word" is a sentence,
# so they are scored, and gated, by characters.
CHARACTER_SCORED = frozenset({"zh", "ja", "ko", "yue"})


def gate(
    records: list[Record],
    statuses: dict[RowKey, tuple[str, str]],
    ceilings: dict[str, dict[str, float]],
) -> list[str]:
    """Every breach of ``ceilings`` ({snap: {language: percent}}); [] passes.

    Each row of a listed snap must score every language listed for it: a
    backend that errors on every clip of a language leaves no rows, and that
    is a failure, never a vacuous pass. So is a row the runner did not finish.
    """
    failures = [
        f"{label}: {status} ({reason})"
        for (_, label), (status, reason) in sorted(statuses.items())
        if status != "ok"
    ]
    warm = [r for r in records if not r.get("cold", False)]
    for snap, limits in ceilings.items():
        keys = sorted(
            {row_key(r) for r in warm if r["label"].split("/")[0].partition("+")[0] == snap}
        )
        if not keys:
            failures.append(f"{snap}: no scored clips")
        for key in keys:
            for language, ceiling in limits.items():
                by_char = language in CHARACTER_SCORED
                edits, total = ("cer_edits", "ref_chars") if by_char else ("wer_edits", "ref_words")
                rows = [r for r in warm if row_key(r) == key and r["language"] == language]
                length = sum(r[total] for r in rows)
                if not length:
                    failures.append(f"{key[1]} {language}: no scored clips")
                    continue
                rate = sum(r[edits] for r in rows) / length * 100
                if rate > ceiling:
                    metric = "CER" if by_char else "WER"
                    failures.append(f"{key[1]} {language}: {metric} {rate:.1f}% > {ceiling:g}%")
    return failures


def _bootstrap() -> Any:
    """The interval module, or a way forward where numpy is missing."""
    try:
        return importlib.import_module("myna.benchmarker._bootstrap")
    except ImportError as exc:
        raise SystemExit(
            f"confidence intervals need numpy ({exc}): install it "
            "(sudo apt install python3-numpy), or pass --no-ci"
        ) from None


def _interval(interval: Any, scale: float = 1.0, spec: str = ".2f") -> str:
    if interval.estimate is None:
        return "--"
    point = format(interval.estimate * scale, spec)
    if interval.low is None:
        return point
    return f"{point} [{interval.low * scale:{spec}}, {interval.high * scale:{spec}}]"


def print_intervals(
    summary: dict[RowKey, SummaryRow],
    records: list[Record],
    order: list[RowKey],
    resamples: int | None = None,
) -> None:
    """95% intervals per row, clips resampled with their repeats."""
    boot = _bootstrap()
    resamples = resamples or boot.RESAMPLES
    groups = _groups(records)
    show_machine = len({key[0] for key in order}) > 1
    rows = [("label", "WER%", "CER%", "WERw%", "CERw%", "RTFx", "med final", "p95 final")]
    for key in order:
        cells = boot.cell_intervals(list(clip_samples(groups[key]).values()), resamples=resamples)
        rows.append(
            (
                f"{key[1]} @ {key[0]}" if show_machine else key[1],
                _interval(cells["wer"], 100),
                _interval(cells["cer"], 100),
                _interval(cells["wer_whisper"], 100),
                _interval(cells["cer_whisper"], 100),
                _interval(cells["rtfx"], spec=".1f"),
                _interval(cells["p50_final"], spec=".3f"),
                TOO_FEW
                if _too_few(summary[key]["timed_clips"], 0.95)
                else _interval(cells["p95_final"], spec=".3f"),
            )
        )
    widths = [max(len(row[i]) for row in rows) for i in range(len(rows[0]))]
    lines = [
        f"{row[0]:{widths[0]}}  "
        + "  ".join(f"{cell:>{w}}" for cell, w in zip(row[1:], widths[1:], strict=True))
        for row in rows
    ]
    print(
        f"\n95% CI: percentile bootstrap over clips ({resamples} resamples, "
        f"seed {boot.SEED}); a clip's repeats are drawn together"
    )
    print("\n".join([lines[0], "-" * len(lines[0]), *lines[1:]]))


def _load_comparable(
    args: argparse.Namespace,
) -> tuple[list[Record], str, dict[RowKey, tuple[str, str]]]:
    """(records, corpus, statuses) of a results file, refused unless its rows
    share one corpus, one normalizer version and one CPU per machine name."""
    records, statuses = _load_latest(Path(args.infile))
    records, corpus = one_corpus(records, args.corpus)
    one_machine_per_name(records)
    one_normalizer_version(records)
    return records, corpus, statuses


def cmd_summarize(args: argparse.Namespace) -> None:
    infile = Path(args.infile)
    if args.ci:
        _bootstrap()  # before the table, not after it
    records, corpus, statuses = _load_comparable(args)
    summary = _summarize(records)
    for key, peaks in _load_resources(resources_path_for(infile)).items():
        if key in summary:
            summary[key]["peak_rss_mb"] = peaks.get("peak_rss_mb")
            summary[key]["peak_vram_mb"] = peaks.get("peak_vram_mb")
            summary[key]["j_per_audio_s"] = peaks.get("j_per_audio_s")
            summary[key]["throttled"] = peaks.get("throttled")
    machines = {key[0] for key in summary}
    print(
        f"{len(records)} records across {len(summary)} row(s) "
        f"on {len(machines)} machine(s) from {infile}"
    )
    print(f"corpus {corpus}\n")
    order = ranked_labels(summary, getattr(args, "sort", "wer") or "wer", statuses)
    _print_overall(summary, order, statuses)
    if args.ci:
        print_intervals(summary, records, order)
    if getattr(args, "by_category", False):
        _print_by_category(records, order, statuses)
    if getattr(args, "gate", None):
        ceilings = yaml.safe_load(Path(args.gate).read_text(encoding="utf-8"))
        failures = gate(records, statuses, ceilings)
        print(f"\ngate {args.gate}: {'FAIL' if failures else 'PASS'}")
        for failure in failures:
            print(f"  {failure}")
        if failures:
            raise SystemExit(1)


def _resolve(name: str, keys: set[RowKey]) -> RowKey:
    """A ``label`` or ``label@machine`` argument as the row it names."""
    label, machine = name.rsplit("@", 1) if "@" in name else (name, "")
    matches = sorted(k for k in keys if k[1] == label and (not machine or k[0] == machine))
    if not matches:
        raise SystemExit(f"no rows labelled {name!r}")
    if len(matches) > 1:
        options = ", ".join(f"{k[1]}@{k[0]}" for k in matches)
        raise SystemExit(f"{name!r} ran on more than one machine; name one of {options}")
    return matches[0]


def _delta(delta: Any, scale: float, spec: str) -> str:
    if delta.estimate is None:
        return "--"
    return (
        f"{delta.estimate * scale:+{spec}} "
        f"[{delta.low * scale:+{spec}}, {delta.high * scale:+{spec}}]  p={delta.p_value:.4f}"
    )


def cmd_compare(args: argparse.Namespace) -> None:
    """Paired bootstrap of two rows over the clips both measured.

    One corpus and one normalizer version, as for the table: a difference
    across either compares the scoring, not the systems.
    """
    records, corpus, _ = _load_comparable(args)
    groups = _groups(records)
    first, second = (_resolve(n, set(groups)) for n in (args.first, args.second))
    boot = _bootstrap()
    try:
        deltas = boot.paired(clip_samples(groups[first]), clip_samples(groups[second]))
    except ValueError as exc:
        raise SystemExit(f"{args.first} vs {args.second}: {exc}") from None
    clips = deltas["wer"].clips
    print(f"{args.first} - {args.second} over {clips} paired clip(s), corpus {corpus}")
    print(f"WER%          {_delta(deltas['wer'], 100, '.2f')}")
    print(f"CER%          {_delta(deltas['cer'], 100, '.2f')}")
    print(f"WERw%         {_delta(deltas['wer_whisper'], 100, '.2f')}")
    print(f"CERw%         {_delta(deltas['cer_whisper'], 100, '.2f')}")
    latency = deltas["p50_final"]
    timed = f"  ({latency.clips} timed on both)" if latency.clips != clips else ""
    print(f"med final s   {_delta(latency, 1, '.3f')}{timed}")
    print(
        f"\n95% CI and two-sided p: paired bootstrap over clips ({boot.RESAMPLES} resamples,"
        f" seed {boot.SEED}); negative means {args.first} is lower."
    )


# ---------------------------------------------------------------------------
# `merge`: fold submissions into the tracked leaderboard
# ---------------------------------------------------------------------------


def _rows_of(path: Path) -> list[Record]:
    """Every non-header record in a results file, in order."""
    if not path.exists():
        raise SystemExit(f"no results at {path}")
    rows: list[Record] = []
    for raw in path.read_text(encoding="utf-8").splitlines():
        raw = raw.strip()
        if raw and json.loads(raw).get("type") != "machine":
            rows.append(json.loads(raw))
    return rows


def cmd_merge(args: argparse.Namespace) -> None:
    """Append submissions to a leaderboard file, replacing each machine's rows.

    A leaderboard is one file tracked in git, so the merge is deliberately
    boring: read what is there, drop everything the incoming machines already
    contributed, append the new rows, write it back. Re-submitting from the
    same machine replaces that machine's rows rather than doubling them, and no
    other machine's rows are touched.

    Three guards, because each failure is silent otherwise. A submission
    measured against a different corpus cannot be compared with what is
    already there; two hosts sharing a hostname would merge into one row and
    lose a submission; and rows scored under different normalizer versions
    would blend two scoring rules into one number.
    """
    out = Path(args.leaderboard)
    existing = _rows_of(out) if out.exists() else []
    incoming: list[Record] = []
    for name in args.results:
        rows = _rows_of(Path(name))
        if not rows:
            print(f"{name}: no records, skipping")
            continue
        incoming += rows

    if not incoming:
        raise SystemExit("nothing to merge")

    corpora = {
        r["corpus_id"]
        for r in incoming + existing
        if "clip" in r and r.get("corpus_id") is not None
    }
    if len(corpora) > 1 and not args.allow_mixed_corpora:
        raise SystemExit(
            "submissions span more than one corpus (" + ", ".join(sorted(corpora)) + ") - a "
            "leaderboard averaged across two corpora ranks nothing. Re-run against the corpus "
            "the leaderboard uses, or pass --allow-mixed-corpora and always summarize with "
            "--corpus <id>."
        )

    one_machine_per_name([r for r in incoming + existing if "clip" in r])
    one_normalizer_version([r for r in incoming + existing if "clip" in r])

    submitting = {machine_of(r) for r in incoming}
    kept = [r for r in existing if machine_of(r) not in submitting]
    replaced = len(existing) - len(kept)

    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w", encoding="utf-8") as fp:
        for row in kept + incoming:
            fp.write(json.dumps(row) + "\n")

    print(f"merged {len(incoming)} record(s) from {', '.join(sorted(submitting))}")
    if replaced:
        print(f"replaced {replaced} earlier record(s) from the same machine(s)")
    print(f"{len(kept) + len(incoming)} record(s) now in {out}")
