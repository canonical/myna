"""Spike S2 (feature 008, T018): NeMo 2.7.3 cache-aware live-push pattern.

Feeds a WAV in realtime-sized pushes through
``CacheAwareStreamingAudioBuffer`` + ``model.conformer_stream_step`` and
prints the per-push accumulated hypothesis with per-push decode cost.

Pinned findings (results/spike-s2-nemo-streaming.md):
  * push pattern: append_audio -> step only FULL chunks on the model's chunk
    schedule mid-stream; flush (partial tail, keep_all_outputs=True) at
    end-of-audio. Sub-chunk steps starve the greedy RNNT's right context and
    silently drop tokens; "peek-and-break" on the buffer generator consumes a
    chunk without stepping it (buffer_idx advances on yield) — same effect.
  * per-step text: decode ``hyp.y_sequence`` via the tokenizer — the
    streaming path never refreshes ``hyp.text`` mid-stream.
  * first ``append_audio`` returns stream_id=-1; pin it to 0 or the next
    append silently grows the batch.

Usage: uv run python dev/spikes/nemo_streaming_feed.py <wav-16k-mono> [push_s] [att_left,att_right]
"""

import sys
import time

import soundfile as sf
import torch

from nemo.collections.asr.models import ASRModel
from nemo.collections.asr.parts.utils.streaming_utils import (
    CacheAwareStreamingAudioBuffer,
)

MODEL = "nvidia/stt_en_fastconformer_hybrid_large_streaming_multi"


def main() -> None:
    wav = sys.argv[1]
    push_s = float(sys.argv[2]) if len(sys.argv) > 2 else 1.0
    att_ctx = [int(x) for x in sys.argv[3].split(",")] if len(sys.argv) > 3 else None

    samples, sr = sf.read(wav, dtype="float32")
    assert sr == 16_000, "expects 16 kHz mono"

    t0 = time.time()
    model = ASRModel.from_pretrained(model_name=MODEL, map_location="cuda")
    model.eval()
    if att_ctx is not None:
        model.encoder.set_default_att_context_size(att_ctx)
    buffer = CacheAwareStreamingAudioBuffer(model)
    cfg = model.encoder.streaming_cfg
    print(
        f"load={time.time()-t0:.1f}s att_ctx={model.encoder.att_context_size} "
        f"chunk={cfg.chunk_size} shift={cfg.shift_size} "
        f"valid_out_len={cfg.valid_out_len}"
    )

    cache_ch, cache_t, cache_len = model.encoder.get_initial_cache_state(batch_size=1)
    prev_hyps = None
    stream_id = -1
    step = 0

    def sched(v, first):
        return v[0] if first and isinstance(v, list) else (v[1] if isinstance(v, list) else v)

    def full_chunks_pending():
        idx, n = buffer.buffer_idx, 0
        rem = int(buffer.streams_length[0]) - idx
        while rem >= sched(cfg.chunk_size, idx == 0):
            n += 1
            idx += sched(cfg.shift_size, idx == 0)
            rem = int(buffer.streams_length[0]) - idx
        return n

    def drain(final: bool) -> None:
        nonlocal prev_hyps, cache_ch, cache_t, cache_len, step
        steps = full_chunks_pending() if not final else None
        it = iter(buffer)
        stepped = 0
        while steps is None or stepped < steps:
            try:
                chunk_audio, chunk_lengths = next(it)
            except StopIteration:
                break
            stepped += 1
            drop = cfg.drop_extra_pre_encoded if step else 0
            with torch.inference_mode():
                (
                    _p,
                    _t,
                    cache_ch,
                    cache_t,
                    cache_len,
                    best,
                ) = model.conformer_stream_step(
                    processed_signal=chunk_audio,
                    processed_signal_length=chunk_lengths,
                    cache_last_channel=cache_ch,
                    cache_last_time=cache_t,
                    cache_last_channel_len=cache_len,
                    keep_all_outputs=final and buffer.is_buffer_empty(),
                    previous_hypotheses=prev_hyps,
                    drop_extra_pre_encoded=drop,
                )
            prev_hyps = best
            step += 1

    def text():
        if not prev_hyps:
            return ""
        return model.tokenizer.ids_to_text([int(t) for t in prev_hyps[0].y_sequence])

    push_len = int(push_s * sr)
    pos = 0
    while pos < len(samples):
        buffer.append_audio(samples[pos : pos + push_len], stream_id)
        stream_id = 0  # first append returns -1 despite creating stream 0
        pos += push_len
        t1 = time.time()
        if full_chunks_pending():
            drain(final=False)
            dt = (time.time() - t1) * 1000
            print(f"[{pos/sr:5.1f}s] {dt:5.0f} ms ...{text()[-60:]!r}")
    t_end = time.time()
    drain(final=True)
    print(f"finalize={(time.time()-t_end)*1000:.0f} ms steps={step}")
    print(f"FINAL: {text()!r}")


if __name__ == "__main__":
    main()
