# Quickstart: Validating Progressive Streaming Emission

**Feature**: `specs/008-progressive-emission`

Runnable scenarios proving the feature end-to-end. The headline validation is
that the **2026-07-27 failing manual test now passes** (SC-006): provisional
(`~`) and committed (`»`) lines arrive *during* realtime playback.

Prerequisites: real corpus fetched (`dev/fetch_real_corpus.py`), model cache
populated (`hf download`, verify `HF_HUB_OFFLINE=1`), `client/` built
(`cargo build --release`). GPU scenarios require the NVIDIA PC.

## S0 — Spikes (gates before adapter work)

- **S1 (CPU)**: run the word-timestamp stability spike over ≥ 10 real-corpus
  clips per `research.md` Decision 3. Record agreement rate / drift; apply the
  go/no-go to the default strategy.
- **S2 (GPU)**: run the NeMo live-feed spike per `research.md` Decision 6.
  Record the push pattern, per-step partial stability, finalize latency at two
  `att_context_size` settings; apply the go/no-go.

## S1 — Whisper strategies (US1)

```sh
myna-server --socket /tmp/myna.sock --adapter whisper --model base --streaming
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --show-unstable \
    --clip corpus/real/audio/librispeech-2277-149896-0005.wav
```

Expected: `~` lines during playback; ≥ 1 `»` line before the clip ends;
`✓` equals the concatenation of `»` lines (I2). (The 2026-07-28 triage
retired tail-mutation/fixed-head after the watermark sweep —
contracts/emission-semantics.md.)

Batch regression: same clip without `--streaming` ⇒ one `»` at end, identical
final text (I7).

## S2 — Nemotron native loop (US2, GPU)

```sh
myna-server --socket /tmp/myna.sock --adapter nemotron --streaming
./client/target/release/myna-dictate --socket /tmp/myna.sock --dialect ie115 \
    --mode streaming --show-unstable --clip <30 s real clip>
```

Expected: continuous `~` partials; `»` commits at natural boundaries; terminal
`✓` within 1 s of clip end (SC-004); repeated with a 5 s clip shows comparable
time-to-first-`»`.

## S3 — Small snaps (US3, US4)

```sh
snap install --dangerous parakeet_*.snap   # and sherpa_*.snap
# point myna-dictate at the snap's session socket (ubustt-socket share, T14c)
./client/target/release/myna-dictate --socket $SNAP_COMMON/ubustt/myna.sock \
    --dialect ie115 --mode streaming --show-unstable --clip <real clip>
```

Expected: confined end-to-end progressive dictation; `du` on the installed
snaps meets SC-005 vs the full NeMo snap.

## S4 — Watermarks and gates

```sh
dev/rebaseline-streaming-watermarks.sh   # whisper: batch + streaming sweep on
                                         # the 26-28 s concatenated streams
```

Expected: `results/streaming-watermarks.json` holds time-to-first-unstable /
time-to-first-committed / finalize-latency per backend×tier; SC-001/003/004
gates evaluated; commit stability 1.0 everywhere (SC-002). Strategy triage
settled 2026-07-28: local-agreement only (SC-001 pass); extend per-backend as
nemotron/parakeet/sherpa land.

## S5 — Concluding report

`docs/interop/streaming-conclusion.md` (SC-007): accuracy, latency profile,
footprint, tier coverage for every backend; recommended backend per hardware
tier. This artifact closes the streaming investigation.
