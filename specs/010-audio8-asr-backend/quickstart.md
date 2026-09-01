# Quickstart: Audio8-ASR Backend Validation

**Feature**: `specs/010-audio8-asr-backend`
**Prerequisites**: repo checkout on branch `010-audio8-asr-backend`; Python 3.12 + `uv`; ~4 GB disk for the staged bundle (dev profile); an Ubuntu desktop session for snap scenarios.

All scenarios assume the spec's acceptance criteria (US1–US3) and reference
`research.md` decisions and `data-model.md` entities rather than restating
them.

## Scenario 1 — Stage the model (US1 prerequisite)

```bash
uv run python dev/fetch_audio8_model.py --accept-license "CC-BY-NC-4.0"
```

Expected: license summary printed and acknowledged; `model_bundle/` (int8
default profile) + engine source staged under the HF cache; script is
idempotent and resumable. Without `--accept-license`, it refuses with the
license notice (FR-014).

## Scenario 2 — Adapter smoke + warm-up lifecycle (US1, SC-003)

```bash
cd server && uv sync --extra audio8
uv run python -m myna.server --adapter audio8 &
```

Expected: logs show `preparing` (load + warm-up) then `ready`; first
real-utterance latency is not measurably slower than subsequent ones.

## Scenario 3 — Dictation over both wire dialects (US1, SC-001)

```bash
myna-dictate --socket /tmp/ubustt.sock --clip server/fixtures/audio/quiet-pangram.wav
myna-dictate --dialect ie115 --socket /tmp/ubustt.sock --clip server/fixtures/audio/quiet-weather.wav
```

Expected: committed transcript returned on both dialects; zero protocol
errors; output contains no special tokens or `language X` prefixes (FR-005);
session is indistinguishable from a whisper-backend session client-side.

## Scenario 4 — Format, length, and silence edges (US1, FR-002/009, SC-005)

- Push 8 kHz audio → rejected per the audio-push invariant (no resampling).
- Push > 30 s audio → decoded via chunk-and-stitch (unbounded, no rejection;
  FR-009 amended).
- Push near-silent audio → empty transcript, no hallucinated text.
- Request an unserved model id → `model_not_available`, no substitution.

## Scenario 5 — Unit + coverage parity (FR-017, SC-007)

```bash
cd server && uv run pytest tests/test_audio8_unit.py        # no weights needed
uv run python ../dev/adapter_coverage.py --adapter audio8   # needs staged model
```

Expected: unit suite green without model weights; merged coverage report
includes `audio8.py` at parity with the funasr adapter floor.

## Scenario 6 — Benchmark + comparison report (US2, SC-002)

```bash
uv run python dev/bench.py --socket /tmp/ubustt.sock --label audio8/cpu            # corpus/real (en)
uv run python dev/bench.py --socket /tmp/ubustt.sock --label audio8/cpu-zh <chinese corpus args>
uv run python dev/aggregate.py --by-category                                       # vs recorded baselines
```

Expected: per-clip JSONL appended to `results/` under the Audio8 labels;
aggregate report ranks Audio8 against whisper/sherpa/parakeet/funasr (+
nemotron/qwen where baselines exist) on WER (en), CER (zh), commit latency,
and RTF; 100% of clips accounted for. Check the comparison into `results/`.

## Scenario 7 — Snap, confined and offline (US3, SC-008)

```bash
# build + install audio8-snap and its model component, connect the myna client snap
sudo snap install --dangerous myna-audio8_*.snap myna-audio8+model-*.comp
sudo snap connect myna:session myna-audio8:session   # content-shared socket
sudo snap disconnect myna-audio8:network 2>/dev/null || true   # prove offline (no network plug exists)
```

Expected: full dictation session succeeds with networking disabled; standard
`preparing → ready` lifecycle; idle-unload via model control; peak memory
within the small-model watermark tolerance. On GPU hardware:
`sudo myna-audio8.audio8 use-engine nvidia-gpu` serves via the CUDA provider and bench
runs take the `audio8/nvidia-gpu` label; selecting it without a GPU fails
fast with a clear error (FR-020).
