"""Spike S1 (feature 008): faster-whisper word-timestamp stability.

LocalAgreement's commit guarantee is only as strong as the stability of what
it compares. This spike re-decodes growing prefixes of real-corpus clips
(2 s steps) with ``word_timestamps=True`` and measures, between adjacent
passes:

- **agreement rate**: fraction of the earlier pass's words (tail-excluded)
  that appear in the later pass's word sequence (order-sensitive alignment);
- **timestamp drift**: |start| delta for agreed words;
- **frontier lag**: how far behind the audio the committed frontier would sit
  at a 1 s cadence (proxy: latest word end agreed by two adjacent passes).

Gate (research.md Decision 3): >= ~90 % agreement and drift ~< 0.3 s ->
local-agreement ships as default strategy; otherwise tail-mutation defaults
and local-agreement falls back to segment-text-prefix agreement.

    uv run python dev/spikes/word_ts_stability.py --model tiny --max-clips 10
    uv run python dev/spikes/word_ts_stability.py --model base --max-clips 10

CPU-runnable; beam_size=1 (what a 1 s-cadence re-decode loop would ship).
Privacy: transcripts never logged, only agreement statistics.
"""

from __future__ import annotations

import argparse
import difflib
import json
import statistics
import sys
import wave
from pathlib import Path

import numpy as np

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
MANIFEST = REPO_ROOT / "corpus" / "real" / "manifest.json"
OUT_MD = REPO_ROOT / "results" / "spike-s1-word-timestamps.md"

STEP_S = 2.0
TAIL_EXCLUDE_S = 0.5  # words ending within this of the window tail carry no right context
MAX_CLIP_S = 30.0
RATE = 16_000


def read_wav_mono_16k(path: Path) -> np.ndarray:
    with wave.open(str(path), "rb") as wf:
        assert wf.getnchannels() == 1, f"{path}: not mono"
        assert wf.getsampwidth() == 2, f"{path}: not s16"
        assert wf.getframerate() == RATE, f"{path}: not {RATE} Hz"
        frames = wf.readframes(wf.getnframes())
    return np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0


def decode_words(model, samples: np.ndarray, beam_size: int) -> list[tuple[str, float, float]]:
    """(word, start, end) tuples for one pass over the window."""
    segments, _ = model.transcribe(
        samples, beam_size=beam_size, word_timestamps=True, vad_filter=False
    )
    words: list[tuple[str, float, float]] = []
    for seg in segments:
        for w in seg.words or []:
            words.append((w.word.strip().lower(), w.start, w.end))
    return words


def agreement(
    prev: list[tuple[str, float, float]],
    curr: list[tuple[str, float, float]],
    prev_window_s: float,
) -> tuple[float, list[float], float]:
    """Compare the earlier pass (tail-excluded) against the later pass.

    Returns (agreement_rate, per-word start drifts, agreed frontier seconds).
    """
    cutoff = prev_window_s - TAIL_EXCLUDE_S
    prev_kept = [w for w in prev if w[2] <= cutoff]
    if not prev_kept:
        return 1.0, [], 0.0
    prev_words = [w[0] for w in prev_kept]
    curr_words = [w[0] for w in curr]
    matcher = difflib.SequenceMatcher(a=prev_words, b=curr_words, autojunk=False)
    drifts: list[float] = []
    matched = 0
    frontier = 0.0
    for tag, i1, i2, j1, j2 in matcher.get_opcodes():
        if tag != "equal":
            continue
        for k in range(i2 - i1):
            matched += 1
            drifts.append(abs(prev_kept[i1 + k][1] - curr[j1 + k][1]))
            frontier = max(frontier, prev_kept[i1 + k][2])
    return matched / len(prev_kept), drifts, frontier


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--model", default="tiny")
    ap.add_argument("--beam-size", type=int, default=1)
    ap.add_argument("--max-clips", type=int, default=10)
    ap.add_argument("--device", default="cpu")
    args = ap.parse_args()

    manifest = json.loads(MANIFEST.read_text())
    # Corpus clips are 2.5–6 s — too short for the 8–30 s protocol. Group by
    # speaker (librispeech-<speaker>-…) and concatenate same-speaker clips
    # with a short gap into virtual utterances of up to MAX_CLIP_S; the spike
    # compares decode passes against *each other*, not a reference, so a
    # spliced stream is valid input.
    by_speaker: dict[str, list[Path]] = {}
    for entry in manifest["clips"]:
        path = REPO_ROOT / "corpus" / "real" / entry["path"]
        if "noise" in path.name:
            continue
        parts = path.stem.split("-")
        speaker = parts[1] if len(parts) > 1 else path.stem
        by_speaker.setdefault(speaker, []).append(path)
    gap = np.zeros(int(0.3 * RATE), dtype=np.float32)
    clips: list[tuple[str, np.ndarray]] = []
    for speaker, paths in sorted(by_speaker.items()):
        acc: list[np.ndarray] = []
        total = 0.0
        for p in sorted(paths):
            s = read_wav_mono_16k(p)
            if acc:
                acc.append(gap)
                total += 0.3
            acc.append(s)
            total += len(s) / RATE
            if total >= MAX_CLIP_S:
                break
        if total >= 8.0:
            clips.append((f"speaker-{speaker}", np.concatenate(acc)[: int(MAX_CLIP_S * RATE)]))
    clips = clips[: args.max_clips]
    if not clips:
        sys.exit("no ≥8s speaker streams found — check corpus/real")

    from faster_whisper import WhisperModel

    print(f"loading faster-whisper {args.model} on {args.device} (beam={args.beam_size})")
    model = WhisperModel(args.model, device=args.device)

    all_rates: list[float] = []
    all_drifts: list[float] = []
    all_lags: list[float] = []
    rows: list[str] = []

    for clip_id, samples in clips:
        duration = min(len(samples) / RATE, MAX_CLIP_S)
        passes: list[tuple[float, list[tuple[str, float, float]]]] = []
        t = STEP_S
        while t <= duration + 1e-6:
            window = samples[: int(t * RATE)]
            passes.append((t, decode_words(model, window, args.beam_size)))
            t += STEP_S
        rates, drifts, lags, n_words = [], [], [], 0
        for (t0, w0), (t1, w1) in zip(passes, passes[1:]):
            rate, d, frontier = agreement(w0, w1, t0)
            rates.append(rate)
            drifts.extend(d)
            lags.append(t1 - frontier if frontier else 0.0)
            n_words += len([w for w in w0 if w[2] <= t0 - TAIL_EXCLUDE_S])
        if not rates or n_words < 20:
            print(f"  {clip_id}: skipped (insufficient words: {n_words})")
            continue
        all_rates.extend(rates)
        all_drifts.extend(drifts)
        all_lags.extend(lags)
        rows.append(
            f"| {clip_id} | {duration:.0f} | {len(rates)} | "
            f"{statistics.mean(rates):.3f} | {min(rates):.3f} | "
            f"{statistics.median(drifts) if drifts else 0:.3f} | "
            f"{statistics.mean(lags):.1f} |"
        )
        print(
            f"  {clip_id}: agreement mean={statistics.mean(rates):.3f} "
            f"min={min(rates):.3f} pairs={len(rates)}"
        )

    if not all_rates:
        sys.exit("no adjacent-pass pairs measured (clips too short?)")

    mean_agree = statistics.mean(all_rates)
    min_agree = min(all_rates)
    med_drift = statistics.median(all_drifts) if all_drifts else 0.0
    p90_drift = sorted(all_drifts)[int(0.9 * len(all_drifts))] if all_drifts else 0.0
    mean_lag = statistics.mean(all_lags) if all_lags else 0.0

    go = mean_agree >= 0.90 and med_drift <= 0.3
    verdict = (
        "**GO** — local-agreement ships as the default strategy"
        if go
        else "**NO-GO** — default flips to tail-mutation; local-agreement uses "
        "segment-text-prefix agreement (research.md Decision 3)"
    )

    report = f"""# Spike S1 findings: faster-whisper word-timestamp stability

**Date**: 2026-07-27
**Model**: faster-whisper `{args.model}` on {args.device}, beam_size={args.beam_size},
word_timestamps=True, vad_filter=False
**Corpus**: {len(rows)} real clips, {STEP_S:.0f}s growing prefixes, tail-excluded {TAIL_EXCLUDE_S}s
**Gate** (research.md Decision 3): >= 90% agreement, median drift <= 0.3s

## Verdict: {verdict}

- **Mean adjacent-pass agreement**: {mean_agree:.3f} (min pair {min_agree:.3f}, n={len(all_rates)})
- **Median timestamp drift (agreed words)**: {med_drift:.3f}s (p90 {p90_drift:.3f}s, n={len(all_drifts)})
- **Mean frontier lag behind audio**: {mean_lag:.1f}s (proxy for committed-frontier lag at 1s cadence)

## Per-clip

| clip | dur (s) | pairs | agreement mean | agreement min | drift median (s) | frontier lag (s) |
|---|---|---|---|---|---|---|
{chr(10).join(rows)}
"""
    OUT_MD.parent.mkdir(parents=True, exist_ok=True)
    # Append per-model section so tiny/base runs accumulate in one document.
    if OUT_MD.exists():
        existing = OUT_MD.read_text()
        marker = f"**Model**: faster-whisper `{args.model}`"
        if marker in existing:
            OUT_MD.write_text(report)
            print(f"wrote {OUT_MD} (replaced {args.model} section)")
            return
        OUT_MD.write_text(existing.rstrip() + "\n\n---\n\n" + report)
    else:
        OUT_MD.write_text(report)
    print(f"wrote {OUT_MD}")


if __name__ == "__main__":
    main()
