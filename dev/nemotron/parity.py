#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "nemo_toolkit[asr]==3.0.0",
#     "torch==2.14.0",
#     "onnx==1.23.2",
#     "onnxruntime==1.27.0",
# ]
#
# [tool.uv]
# # lightning 2.4.0, the newest NeMo allows, caps fsspec below 2026, and every
# # fsspec before 2026.6.0 renders ReferenceFileSystem templates unsafely.
# override-dependencies = ["fsspec[http]>=2026.6.0"]
#
# [tool.uv.sources]
# torch = { index = "pytorch-cpu" }
#
# [[tool.uv.index]]
# name = "pytorch-cpu"
# url = "https://download.pytorch.org/whl/cpu"
# explicit = true
# ///
"""Check an ONNX Nemotron export against NeMo's own cache-aware streaming.

    parity.py EXPORT_DIR MODEL.nemo CLIP.wav [--int8] [--seconds N] [--text OUT]
              [--mel GRAPH.onnx]

Runs in the NeMo environment. Both sides get the same mel (NeMo's
preprocessor, dither off) and step the same chunks with
``pad_and_drop_preencoded`` semantics (every chunk the steady size, a zero
pre-encode cache before the first, the extra pre-encoded frames always
dropped), which is what the exported encoder bakes in. Prints the largest
encoder-output difference per step and both transcripts.
"""

import argparse
import json
import time
import wave
from pathlib import Path

import nemo.collections.asr as nemo_asr
import numpy as np
import onnxruntime as ort
import torch

parser = argparse.ArgumentParser()
parser.add_argument("export", type=Path)
parser.add_argument("nemo")
parser.add_argument("clip")
parser.add_argument("--int8", action="store_true")
parser.add_argument("--seconds", type=float, default=60.0)
parser.add_argument("--threads", type=int, default=2)
parser.add_argument("--text", type=Path, help="write the ONNX transcript here")
parser.add_argument("--mel", type=Path, help="an ONNX log-mel graph for the ONNX side")
args = parser.parse_args()

config = json.loads((args.export / "config.json").read_text())
stream = config["streaming"]
chunk, pre_cache = stream["chunk_size"][1], stream["pre_encode_cache_size"][1]
drop = stream["drop_extra_pre_encoded"]

with wave.open(args.clip) as w:
    assert w.getframerate() == 16000 and w.getnchannels() == 1
    pcm = np.frombuffer(w.readframes(int(args.seconds * 16000)), "<i2")
audio = np.concatenate([pcm.astype(np.float32) / 32768, np.zeros(32000, np.float32)])

model = nemo_asr.models.ASRModel.restore_from(args.nemo, map_location="cpu").eval()
model.encoder.set_default_att_context_size(config["att_context_size"])
model.preprocessor.featurizer.dither = 0.0
torch.set_num_threads(args.threads)
with torch.inference_mode():
    mel, _ = model.preprocessor(
        input_signal=torch.from_numpy(audio).unsqueeze(0),
        length=torch.tensor([len(audio)]),
    )
nemo_mel = mel[0].numpy()  # (128, frames)
onnx_mel = nemo_mel
if args.mel:
    graph = ort.InferenceSession(str(args.mel))
    (feats,) = graph.run(
        ["features"],
        {"waveforms": audio[None], "waveforms_lens": np.array([len(audio)], np.int64)},
    )
    onnx_mel = feats[0, :, : nemo_mel.shape[1]]
    print(f"mel max abs diff {float(np.abs(onnx_mel - nemo_mel).max()):.4g}")
steps = (nemo_mel.shape[1]) // chunk


def chunk_at(i: int, mel: np.ndarray = nemo_mel) -> np.ndarray:
    start = i * chunk
    before = mel[:, max(0, start - pre_cache) : start]
    pad = np.zeros((mel.shape[0], pre_cache - before.shape[1]), np.float32)
    return np.concatenate([pad, before, mel[:, start : start + chunk]], axis=1)[None]


# NeMo: the encoder output on its own, then the full step (encoder again,
# then greedy RNNT) from the same caches.
ch, tm, ln = model.encoder.get_initial_cache_state(batch_size=1)
hyps, pred = None, None
ref_enc = []
nemo_cpu = 0.0
with torch.inference_mode():
    for i in range(steps):
        x = torch.from_numpy(chunk_at(i))
        n = torch.tensor([x.shape[-1]])
        enc, enc_len, *_ = model.encoder.cache_aware_stream_step(
            processed_signal=x,
            processed_signal_length=n,
            cache_last_channel=ch,
            cache_last_time=tm,
            cache_last_channel_len=ln,
            keep_all_outputs=False,
            drop_extra_pre_encoded=drop,
        )
        ref_enc.append(enc[0, :, : int(enc_len[0])].numpy())
        t0 = time.process_time()
        pred, _texts, ch, tm, ln, hyps = model.conformer_stream_step(
            processed_signal=x,
            processed_signal_length=n,
            cache_last_channel=ch,
            cache_last_time=tm,
            cache_last_channel_len=ln,
            keep_all_outputs=False,
            previous_hypotheses=hyps,
            previous_pred_out=pred,
            drop_extra_pre_encoded=drop,
            return_transcription=True,
        )
        nemo_cpu += time.process_time() - t0
nemo_tokens = hyps[0].y_sequence.tolist()

# ONNX
suffix = ".int8.onnx" if args.int8 else ".onnx"
opts = ort.SessionOptions()
opts.intra_op_num_threads = args.threads
opts.inter_op_num_threads = 1
encoder = ort.InferenceSession(str(args.export / f"encoder-model{suffix}"), opts)
one = ort.SessionOptions()
one.intra_op_num_threads = 1
one.inter_op_num_threads = 1
joint = ort.InferenceSession(str(args.export / f"decoder_joint-model{suffix}"), one)
# NeMo keeps its caches layer-first; the export takes them batch-first.
ref_cache = model.encoder.get_initial_cache_state(batch_size=1)
ch_o = np.zeros(tuple(ref_cache[0].transpose(0, 1).shape), np.float32)
tm_o = np.zeros(tuple(ref_cache[1].transpose(0, 1).shape), np.float32)
ln_o = np.zeros(tuple(ref_cache[2].shape), np.int64)
blank = joint.get_outputs()[0].shape[-1] - 1
max_symbols = model.cfg.decoding.greedy.get("max_symbols", 10) or 10
tokens: list[int] = []
prev = blank
state = (np.zeros((2, 1, 640), np.float32), np.zeros((2, 1, 640), np.float32))
worst = 0.0
onnx_cpu = 0.0
for i in range(steps):
    x = chunk_at(i, onnx_mel)
    t0 = time.process_time()
    enc, enc_len, ch_o, tm_o, ln_o = encoder.run(
        None,
        {
            "audio_signal": x,
            "length": np.array([x.shape[-1]], np.int64),
            "cache_last_channel": ch_o,
            "cache_last_time": tm_o,
            "cache_last_channel_len": ln_o,
        },
    )
    frames = enc[0, :, : int(enc_len[0])]
    for t in range(frames.shape[1]):
        frame = frames[None, :, t : t + 1]
        for _ in range(max_symbols):
            logits, _len, s1, s2 = joint.run(
                None,
                {
                    "encoder_outputs": frame,
                    "targets": np.array([[prev]], np.int32),
                    "target_length": np.array([1], np.int32),
                    "input_states_1": state[0],
                    "input_states_2": state[1],
                },
            )
            token = int(np.argmax(logits.reshape(-1)))
            if token == blank:
                break
            tokens.append(token)
            prev, state = token, (s1, s2)
    onnx_cpu += time.process_time() - t0
    ref = ref_enc[i]
    if ref.shape != frames.shape:
        print(f"step {i}: shape {frames.shape} vs NeMo {ref.shape}")
    else:
        worst = max(worst, float(np.abs(ref - frames).max()))

text = model.tokenizer.ids_to_text
print(f"steps {steps}, encoder max abs diff {worst:.4g}")
print(
    f"tokens identical: {tokens == nemo_tokens} ({len(tokens)} vs {len(nemo_tokens)})"
)
print(
    f"cpu s: nemo {nemo_cpu:.1f}, onnx {onnx_cpu:.1f} for {len(pcm) / 16000:.0f} s audio"
)
print("NeMo:", text(nemo_tokens))
print("ONNX:", text(tokens))
if args.text:
    args.text.write_text(text(tokens) + "\n")
