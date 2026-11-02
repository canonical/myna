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
"""Export a cache-aware streaming Nemotron checkpoint to ONNX for Myna's
torch-free loop.

    dev/nemotron/export_nemotron_onnx.py MODEL.nemo OUT_DIR RIGHT_CONTEXT

Covers nvidia/nemotron-speech-streaming-en-0.6b and
nvidia/nemotron-3.5-asr-streaming-0.6b. RIGHT_CONTEXT is the encoder's
right context in 80 ms frames (6 = 0.56 s chunks, 13 = 1.12 s); the graph
is fixed to it.

OUT_DIR receives:

- ``encoder-model.onnx`` and ``decoder_joint-model.onnx`` (fp32, external
  weights), from NeMo's exporter with ``cache_support``, and their
  ``.int8.onnx`` twins (dynamic; the encoder's MatMul weights only).
- ``prompt-model.onnx``, for a model with a language prompt only: NeMo's
  exporter drops ``PromptStreamingMixin._apply_prompt_to_encoded``.
- ``vocab.txt``, ``config.json``: tokens (blank last), the streaming
  configuration, the preprocessor's settings and the language prompts.
"""

import json
import shutil
import sys
import tempfile
import time
from pathlib import Path

import nemo.collections.asr as nemo_asr
import onnx
import onnxruntime as ort
import torch
from onnxruntime.quantization import QuantType, quantize_dynamic

nemo_path, out, right = sys.argv[1], Path(sys.argv[2]), int(sys.argv[3])
out.mkdir(parents=True, exist_ok=True)

model = nemo_asr.models.ASRModel.restore_from(nemo_path, map_location="cpu").eval()
left = model.encoder.att_context_size[0]
model.encoder.set_default_att_context_size([left, right])
cfg = model.encoder.streaming_cfg

# NeMo's exporter scatters weights into one file per tensor; gather each
# graph's into one beside it. The quantiser's scratch copies go to disk, not
# a tmpfs /tmp: three of them are 7 GB.
tempfile.tempdir = str(out)
raw = out / "raw"
raw.mkdir(exist_ok=True)
t0 = time.time()
model.set_export_config({"cache_support": "True"})
model.export(str(raw / "model.onnx"))
for part in ("encoder", "decoder_joint"):
    name = f"{part}-model.onnx"
    onnx.save_model(
        onnx.load(str(raw / name)),
        str(out / name),
        save_as_external_data=True,
        all_tensors_to_one_file=True,
        location=f"{name}.data",
    )
shutil.rmtree(raw)
print("exported in", f"{time.time() - t0:.0f}s", flush=True)

prompts = None
if hasattr(model, "prompt_kernel"):
    prompts = dict(model.cfg.model_defaults.prompt_dictionary)

    class PromptProjection(torch.nn.Module):
        """Encoder frames (B, D, T) and a one-hot language (B, P) to
        conditioned frames (B, D, T)."""

        def __init__(self, kernel: torch.nn.Module) -> None:
            super().__init__()
            self.kernel = kernel

        def forward(self, encoded: torch.Tensor, prompt: torch.Tensor) -> torch.Tensor:
            frames = encoded.transpose(1, 2)
            expanded = prompt.unsqueeze(1).expand(-1, frames.shape[1], -1)
            return self.kernel(torch.cat([frames, expanded], dim=-1)).transpose(1, 2)

    proj = PromptProjection(model.prompt_kernel).eval()
    enc = torch.randn(1, model.cfg.model_defaults.enc_hidden, 7)
    prompt = torch.zeros(1, model.num_prompts)
    prompt[0, 0] = 1.0
    torch.onnx.export(
        proj,
        (enc, prompt),
        str(out / "prompt-model.onnx"),
        input_names=["encoder_outputs", "prompt"],
        output_names=["outputs"],
        dynamic_axes={
            "encoder_outputs": {0: "B", 2: "T"},
            "prompt": {0: "B"},
            "outputs": {0: "B", 2: "T"},
        },
        opset_version=17,
        dynamo=False,
    )
    got = ort.InferenceSession(str(out / "prompt-model.onnx")).run(
        None, {"encoder_outputs": enc.numpy(), "prompt": prompt.numpy()}
    )[0]
    with torch.no_grad():
        ref = proj(enc, prompt).numpy()
    print("prompt graph max abs diff:", float(abs(got - ref).max()), flush=True)


def plain(value):
    return value.tolist() if hasattr(value, "tolist") else value


with (out / "vocab.txt").open("w", encoding="utf-8") as fh:
    for i, token in enumerate([*model.tokenizer.vocab, "<blk>"]):
        fh.write(f"{token} {i}\n")
(out / "config.json").write_text(
    json.dumps(
        {
            "source": Path(nemo_path).name,
            "att_context_size": [left, right],
            "streaming": {k: plain(v) for k, v in vars(cfg).items()},
            "preprocessor": {k: plain(v) for k, v in dict(model.cfg.preprocessor).items()},
            "subsampling_factor": model.encoder.subsampling_factor,
            # Batch-first, as the export takes them; NeMo keeps them layer-first.
            "cache_shapes": dict(
                zip(
                    ("cache_last_channel", "cache_last_time", "cache_last_channel_len"),
                    (
                        [1, *c.shape[:1], *c.shape[2:]] if c.dim() > 1 else list(c.shape)
                        for c in model.encoder.get_initial_cache_state(batch_size=1)
                    ),
                    strict=True,
                )
            ),
            "prompts": prompts,
        },
        default=str,
        indent=1,
    )
)

for part in ("encoder", "decoder_joint"):
    src = out / f"{part}-model.onnx"
    t0 = time.time()
    # The encoder's MatMuls only: quantising its 77 Conv layers as well cost
    # alestro 0.3 WER points and ran slower (0.241 core against 0.205),
    # measured 2026-11-02. The decoder_joint keeps every op (its LSTM too).
    quantize_dynamic(
        src,
        out / f"{part}-model.int8.onnx",
        weight_type=QuantType.QInt8,
        op_types_to_quantize=["MatMul"] if part == "encoder" else None,
    )
    print("quantised", part, f"{time.time() - t0:.0f}s", flush=True)

for name in sorted(p.name for p in out.iterdir() if p.suffix == ".onnx"):
    m = onnx.load(str(out / name), load_external_data=False)

    def dims(values):
        return [
            (v.name, [d.dim_value or d.dim_param for d in v.type.tensor_type.shape.dim])
            for v in values
        ]

    print(name, "in:", dims(m.graph.input), "out:", dims(m.graph.output))
