#!/usr/bin/env python3
"""Does Whisper invent text when there is nothing to transcribe?

The corpus tiers cannot answer this. Every clip in `corpus/real` is a person
reading, so a decoder setting that suppresses spurious output can only ever
look neutral or harmful there - which is exactly what the clean-corpus sweep
in dev/lab/whisper_decode_sweep.py shows. The failure this probes for is the
other one: near-silence decoded into training-data boilerplate ("Thank you for
watching"), which is what a real dictation looks like when the user taps the
hotkey twice, or speaks a single short word into a 30 s window.

OpenWhispr report the whisper.cpp pair `entropy_thold 2.8 / logprob_thold
-1.25` (from 2.4 / -1.0) cutting their hallucinated-tail rate from 2.25% to
0.06% over 4,814 real dictations (their #1458). This scores the faster-whisper
analogues on inputs whose correct transcript is known to be **empty**, so any
output at all is a false positive and the metric is unambiguous.

Cases, all 16 kHz mono, all with an empty reference:

  silence-Ns          digital silence
  dither-Ns           -70 dBFS noise (a real mic's noise floor, not zeros -
                      CTranslate2's VAD-free path treats exact zeros unusually)
  roomtone-Ns         -45 dBFS pink-ish noise (a quiet room)
  hum-Ns              -50 dBFS 50 Hz mains hum plus dither

...plus, with a non-empty reference, the mixed case that matters most in
practice: a real corpus clip padded with leading and trailing silence, where
the correct answer is the clip's own transcript and nothing else.

    uv run --extra whisper python dev/lab/whisper_silence_probe.py \\
        --model whisper-snap/components/model-tiny-ct2
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import wave
from dataclasses import asdict, dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
# The import below needs this path first, which is what E402 is waived for.
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

from myna.testbed.metrics import word_error_rate  # noqa: E402

# Shared with dev/lab/whisper_decode_sweep.py; kept in one place so a config
# named in one tool means the same thing in the other.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from whisper_decode_sweep import DECODE_CONFIGS  # noqa: E402

RATE = 16_000


def _rng(seed: int):
    import numpy as np

    return np.random.default_rng(seed)


def dbfs_to_amp(dbfs: float) -> float:
    return 10.0 ** (dbfs / 20.0)


def build_empty_cases(durations, seed: int):
    """Cases whose correct transcript is the empty string."""
    import numpy as np

    cases = []
    for i, secs in enumerate(durations):
        n = int(secs * RATE)
        rng = _rng(seed + i)
        cases.append((f"silence-{secs}s", np.zeros(n, dtype=np.float32)))
        cases.append(
            (
                f"dither-{secs}s",
                (rng.standard_normal(n) * dbfs_to_amp(-70)).astype(np.float32),
            )
        )
        # Pink-ish: a one-pole low-pass over white noise. Close enough to room
        # tone for this purpose, and deterministic.
        white = rng.standard_normal(n)
        pink = np.empty(n)
        acc = 0.0
        for j, v in enumerate(white):
            acc = 0.97 * acc + 0.03 * v
            pink[j] = acc
        pink = pink / (np.abs(pink).max() or 1.0) * dbfs_to_amp(-45)
        cases.append((f"roomtone-{secs}s", pink.astype(np.float32)))
        t = np.arange(n) / RATE
        hum = np.sin(2 * np.pi * 50 * t) * dbfs_to_amp(-50)
        hum = hum + rng.standard_normal(n) * dbfs_to_amp(-70)
        cases.append((f"hum-{secs}s", hum.astype(np.float32)))
    return cases


def build_padded_cases(manifest: Path, count: int, pad_seconds: float):
    """Real speech with silence either side: correct answer is the clip's own
    transcript, and only that."""
    import numpy as np

    data = json.loads(manifest.read_text())
    clips = [c for c in data["clips"] if c.get("category") == "quiet"][:count]
    pad = np.zeros(int(pad_seconds * RATE), dtype=np.float32)
    out = []
    for c in clips:
        with wave.open(str(manifest.parent / c["path"]), "rb") as w:
            pcm = w.readframes(w.getnframes())
        samples = np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0
        out.append((f"padded-{c['id']}", np.concatenate([pad, samples, pad]), c["text"]))
    return out


@dataclass
class Row:
    decode_config: str
    empty_cases: int
    empty_with_output: int
    empty_output_chars: int
    padded_cases: int
    padded_wer: float
    padded_extra_words: int
    decode_seconds: float


def main() -> int:
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument(
        "--model",
        default="tiny",
        help="model size name or a CTranslate2 model directory",
    )
    p.add_argument("--compute-type", default="default")
    p.add_argument("--decode-configs", nargs="+", default=sorted(DECODE_CONFIGS))
    p.add_argument("--durations", nargs="+", type=float, default=[1.0, 5.0, 30.0])
    p.add_argument("--padded-clips", type=int, default=8)
    p.add_argument("--pad-seconds", type=float, default=3.0)
    p.add_argument(
        "--manifest",
        type=Path,
        default=REPO_ROOT / "corpus/real/manifest-balanced.json",
    )
    p.add_argument("--seed", type=int, default=20260826)
    p.add_argument("--out", type=Path, default=REPO_ROOT / "results/whisper-silence-probe.json")
    args = p.parse_args()

    from faster_whisper import WhisperModel

    empty_cases = build_empty_cases(args.durations, args.seed)
    padded_cases = build_padded_cases(args.manifest, args.padded_clips, args.pad_seconds)
    print(
        f"{len(empty_cases)} empty-reference cases, {len(padded_cases)} padded-speech cases",
        file=sys.stderr,
        flush=True,
    )

    model = WhisperModel(args.model, device="cpu", compute_type=args.compute_type)

    report = {
        "model": args.model,
        "compute_type": args.compute_type,
        "durations": args.durations,
        "pad_seconds": args.pad_seconds,
        "rows": [],
        "transcripts": {},
    }

    for name in args.decode_configs:
        overrides = DECODE_CONFIGS[name]
        with_output = chars = 0
        transcripts: dict[str, str] = {}
        t0 = time.perf_counter()
        for case, samples in empty_cases:
            segments, _ = model.transcribe(samples, language="en", **overrides)
            text = "".join(s.text for s in segments).strip()
            if text:
                with_output += 1
                chars += len(text)
                transcripts[case] = text

        wer_edits = wer_ref = extra = 0
        for case, samples, reference in padded_cases:
            segments, _ = model.transcribe(samples, language="en", **overrides)
            text = "".join(s.text for s in segments).strip()
            w = word_error_rate(reference, text)
            wer_edits += w.substitutions + w.deletions + w.insertions
            wer_ref += w.reference_length
            extra += w.insertions
            if w.insertions:
                transcripts[case] = text
        elapsed = time.perf_counter() - t0

        row = Row(
            decode_config=name,
            empty_cases=len(empty_cases),
            empty_with_output=with_output,
            empty_output_chars=chars,
            padded_cases=len(padded_cases),
            padded_wer=round(wer_edits / wer_ref, 5) if wer_ref else 0.0,
            padded_extra_words=extra,
            decode_seconds=round(elapsed, 2),
        )
        report["rows"].append(asdict(row))
        report["transcripts"][name] = transcripts
        print(
            f"  {name:<32} empty-with-output {with_output}/{len(empty_cases)} "
            f"({chars} chars)  padded WER {row.padded_wer:.4f} (+{extra} words)",
            file=sys.stderr,
            flush=True,
        )

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2))

    hdr = f"{'decode config':<32} {'empty->text':>12} {'chars':>7} {'padded WER':>11} {'+words':>7}"
    print("\n" + hdr)
    print("-" * len(hdr))
    for r in report["rows"]:
        print(
            f"{r['decode_config']:<32} "
            f"{r['empty_with_output']:>5}/{r['empty_cases']:<6} "
            f"{r['empty_output_chars']:>7} {r['padded_wer']:>11.4f} "
            f"{r['padded_extra_words']:>7}"
        )
    print(f"\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
