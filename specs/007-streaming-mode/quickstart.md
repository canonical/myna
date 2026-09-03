# Quickstart: Validating Dual-Mode Streaming Transcription

**Date**: 2026-07-27
**Feature**: `specs/007-streaming-mode`

## Prerequisites

- Rust toolchain (stable, via Workshop: `workshop launch myna`)
- Python environment with `uv` (`uv sync --extra whisper --extra nemotron`)
- A running `myna-server` with a streaming-capable adapter
- Real corpus available (`dev/fetch_english_corpus.py` run at least once)
- (Optional) The canonical/whisper-snap adapter + WhisperLive docker for interop testing

## Scenario 1: Streaming committed segments (Nemotron, native transducer)

**Purpose**: Prove that committed text arrives progressively before end-of-audio.

```bash
# Terminal 1: start the server in streaming mode
myna-server --adapter nemotron --socket /tmp/myna-stream.sock --streaming

# Terminal 2: dictate a long clip (≥8s)
./client/target/release/myna-dictate \
  --socket /tmp/myna-stream.sock --dialect ie115 \
  --clip corpus/english/audio/librispeech-2277-149896-0005.wav

# Expected: committed text lines (» prefix) appear DURING playback, before the
# clip finishes. Final ✓ line shows the complete transcript.
```

**Expected output shape**:
```
── clip corpus/english/audio/librispeech-2277-149896-0005.wav
🎤 ready — listening (streaming)
   » Many little wrinkles
   » gathered between his eyes
   » as he contemplated this
✓ Many little wrinkles gathered between his eyes as he contemplated this and his brow moistened.
```

**Validation**: At least one `»` line appears before the clip's realtime duration
elapses. The final transcript matches batch mode output (WER within 2pp).

## Scenario 2: Batch mode on low-tier hardware (CPU Whisper)

**Purpose**: Prove that batch mode is unchanged and the tier gate works.

```bash
# Start server WITHOUT --streaming (or with a model whose tier is batch-only)
myna-server --adapter whisper --model tiny --socket /tmp/myna-batch.sock

# Dictate the same clip
./client/target/release/myna-dictate \
  --socket /tmp/myna-batch.sock --dialect ie115 \
  --clip corpus/english/audio/librispeech-2277-149896-0005.wav

# Expected: NO progressive text. Single ✓ line appears after full inference.
```

**Expected output shape**:
```
── clip corpus/english/audio/librispeech-2277-149896-0005.wav
🎤 ready — listening
✓ Many little wrinkles gathered between his eyes as he contemplated this and his brow moistened.
```

**Validation**: No `»` lines. Text appears only after the clip ends and inference
completes. Behavior identical to today.

## Scenario 3: Wire protocol validation (disposition field)

**Purpose**: Prove the committed/unstable discriminant is on the wire.

```bash
# Use the Python test client to dump raw events
python3 dev/transcribe.py --socket /tmp/myna-stream.sock \
  --clip corpus/english/audio/librispeech-2277-149896-0005.wav \
  --raw-events

# Expected: delta events carry "disposition": "committed" or "unstable"
```

**Validation**: Every `conversation.item.input_audio_transcription.delta` event
has a `disposition` field. Committed events are append-only (verify no committed
text is contradicted by a later event).

## Scenario 4: Interop with canonical/whisper-snap adapter

**Purpose**: Prove our client handles a third-party IE115 server's streaming output.

```bash
# Terminal 1: WhisperLive backend. Docker needs root; a local venv does not,
# and `reference/WhisperLive` is already checked out:
cd reference/WhisperLive
uv venv --python 3.12 .venv
VIRTUAL_ENV=$PWD/.venv uv pip install --torch-backend=cpu \
  faster-whisper==1.2.0 websockets 'onnxruntime>=1.20.0,<2' numba kaldialign \
  soundfile scipy av 'numpy>=1.26.4,<2.5' openai-whisper==20250625 \
  tokenizers==0.20.3 transformers torch sentencepiece librosa \
  fastapi uvicorn python-multipart
CUDA_VISIBLE_DEVICES="" ./.venv/bin/python run_server.py --port 9090 \
  --backend faster_whisper --max_connection_time 3600 --omp_num_threads "$(nproc)"
# (this checkout's run_server.py has no --host; the snap ships its own
#  scripts/whisper-live-server.py wrapper that adds one. It binds 0.0.0.0.)
# Docker equivalent, if you have root:
#   sudo docker run --rm -p 9090:9090 ghcr.io/collabora/whisperlive-cpu:latest

# Terminal 2: The Go adapter
cd reference/whisper-snap
go run ./cmd/whisperlive-adapter serve \
  --unix-socket /tmp/myna-adapter.sock \
  --model base --language en \
  --allowed-models "small,base,tiny" --allowed-languages "auto,en"

# Terminal 3: Our client. NOTE: no --language. Sending one triggers their
# unconditional backend reload and costs the first ~600ms of audio (gap 5/6).
./client/target/release/myna-dictate \
  --socket /tmp/myna-adapter.sock --dialect ie115 --base64-audio \
  --ws-path /v1/realtime \
  --clip corpus/english/audio/librispeech-2277-149896-0005.wav

# Expected: transcription completes (possibly with delta text shown progressively
# depending on clip length and WhisperLive's segment timing).
```

**Validation**: Session completes without errors. Transcript is recognizable
(may differ from our whisper snap due to model/quantization differences). The
6 interop fixes (ws-path, model.loaded mapping, empty-completed handling,
session.update skip, model.unloaded mapping, empty-transcription omission) are
exercised.

> **Use a single-utterance clip.** Their `completed` fires per VAD segment, not
> per commit, so any clip containing a pause ends our session early and truncates
> the rest (2026-08-20, `docs/interop/canonical-whisper-snap-report.md`). A
> multi-utterance clip is the reproducer for that finding, not a passing run.

## Scenario 5: Mode override setting

**Purpose**: Prove the user can force batch mode on a streaming-capable tier.

```bash
# Set mode to batch explicitly
# (mechanism TBD: dconf key, snap config, or CLI flag)
myna-dictate --mode batch --socket /tmp/myna-stream.sock ...

# Expected: even though the server advertises streaming=true, the client
# operates in batch display mode (accumulates all text, shows only at end).
```

**Validation**: No progressive `»` lines despite the server being streaming-capable.

## Metrics to capture (dev/matrix.py)

After implementation, run the matrix with `--streaming` targets and verify:

| Metric | Streaming target | Batch target |
|--------|-----------------|--------------|
| `time_to_first_committed` | ≤ 3s (Nemotron) / ≤ 5s (Whisper) | N/A |
| `wer` | Within 2pp of batch | Unchanged |
| `rtf` | < 1.0 (confirmed viable) | > 1.0 or irrelevant |
| `commit_stability` | 100% (no retraction of committed text) | N/A |

## Measured results (2026-07-27, whisper-tiny, x86_64 CPU, real corpus)

Verified with `dev/bench.py --streaming` against `myna-server --adapter
whisper --model tiny --streaming`:

| Metric | Measured | Notes |
|--------|----------|-------|
| `committed_segments` | 2 (6.3s clip) | sentence-split segments |
| `commit_stability` | true (100%) | SC-004 holds across the sweep |
| `wer` (streaming) | 0.00% | identical transcript to batch — SC-002 holds |
| `rtf` | 1.08 | correctly gates this CPU tier to batch (FR-002) |
| `time_to_first_committed` | ≈ audio duration | see FR-008 gap note in docs/architecture/streaming.md |

Mode behaviors verified end-to-end (same clip, `librispeech-2277-149896-0030`):

- `--mode streaming` → 2 progressive `»` lines before the `✓` terminal
- `--mode batch` → no `»` lines, single `✓` terminal (US2 acceptance)
- `--mode auto` → resolves to batch on this CPU (RTF 1.08 > 1.0 in the
  shipped baseline, results/streaming-tiers.json)
- `--show-unstable` → unstable hypothesis deltas render as `~` lines

Interop (canonical/whisper-snap adapter + WhisperLive docker, 2026-07-27):
session completes; their deltas restate the growing hypothesis without a
disposition field (backward-compat → committed) — the finding documented in
docs/interop/canonical-whisper-snap-report.md. Re-run:
`cargo test -p myna-cli --test interop_canonical -- --ignored`.

Re-run 2026-08-20 against adapter HEAD `8ae643b`: still true, plus their
endpoint moved to `/v1/realtime` and their `completed` turns out to be
per-VAD-segment rather than per-commit, which truncates any dictation
containing a pause. See the "Re-run" section of the same report.
