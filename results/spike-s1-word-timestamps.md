# Spike S1 findings: faster-whisper word-timestamp stability

**Date**: 2026-07-27
**Model**: faster-whisper `tiny` on cpu, beam_size=1,
word_timestamps=True, vad_filter=False
**Corpus**: 1 real clips, 2s growing prefixes, tail-excluded 0.5s
**Gate** (research.md Decision 3): >= 90% agreement, median drift <= 0.3s

## Verdict: **GO** — local-agreement ships as the default strategy

- **Mean adjacent-pass agreement**: 0.997 (min pair 0.975, n=14)
- **Median timestamp drift (agreed words)**: 0.000s (p90 0.020s, n=615)
- **Mean frontier lag behind audio**: 2.8s (proxy for committed-frontier lag at 1s cadence)

## Per-clip

| clip | dur (s) | pairs | agreement mean | agreement min | drift median (s) | frontier lag (s) |
|---|---|---|---|---|---|---|
| speaker-2277 | 30 | 14 | 0.997 | 0.975 | 0.000 | 2.8 |

---

# Spike S1 findings: faster-whisper word-timestamp stability

**Date**: 2026-07-27
**Model**: faster-whisper `base` on cpu, beam_size=1,
word_timestamps=True, vad_filter=False
**Corpus**: 1 real clips, 2s growing prefixes, tail-excluded 0.5s
**Gate** (research.md Decision 3): >= 90% agreement, median drift <= 0.3s

## Verdict: **GO** — local-agreement ships as the default strategy

- **Mean adjacent-pass agreement**: 0.982 (min pair 0.900, n=14)
- **Median timestamp drift (agreed words)**: 0.000s (p90 0.000s, n=610)
- **Mean frontier lag behind audio**: 2.8s (proxy for committed-frontier lag at 1s cadence)

## Per-clip

| clip | dur (s) | pairs | agreement mean | agreement min | drift median (s) | frontier lag (s) |
|---|---|---|---|---|---|---|
| speaker-2277 | 30 | 14 | 0.982 | 0.900 | 0.000 | 2.8 |
