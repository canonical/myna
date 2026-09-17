#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12,<3.13"
# dependencies = [
#     "nemo_toolkit[asr]==3.0.0",
#     "torch==2.14.0",
#     "onnx==1.22.0",
#     "onnxruntime==1.20.1",
# ]
#
# [tool.uv.sources]
# torch = { index = "pytorch-cpu" }
#
# [[tool.uv.index]]
# name = "pytorch-cpu"
# url = "https://download.pytorch.org/whl/cpu"
# explicit = true
# ///
"""Export nvidia/nemotron-3.5-asr-streaming-0.6b (rev ea30d66, OpenMDW-1.1)
to ONNX with NeMo's own cache-aware streaming exporter.

    dev/eval/export_nemotron35_onnx.py MODEL.nemo OUT_DIR RIGHT_CONTEXT

``MODEL.nemo`` is a local checkpoint (this script does not fetch one).
``RIGHT_CONTEXT`` is the encoder's right-context frame count (13 in the
2026-09-17 evaluation); it sets ``att_context_size`` before export.

Three artifacts land in ``OUT_DIR``:

- ``model.onnx`` (+ external weights): the cache-aware encoder (cache I/O
  ports baked in) and decoder_joint, from
  ``model.set_export_config({"cache_support": "True"})`` followed by
  ``model.export()``. NeMo's exporter drops the language-prompt projection,
  so it is not in this graph.
- ``prompt-model.onnx``: the language-prompt projection
  (``PromptStreamingMixin._apply_prompt_to_encoded``), exported separately
  since NeMo's own exporter does not cover it. Checked against the torch
  module's output after export (printed as "prompt graph max abs diff").
- ``vocab.txt``, ``prompts.json``, ``streaming.json``: tokenizer vocabulary,
  the language-to-prompt-index mapping, and the encoder's streaming
  configuration (chunk/shift/cache sizes, ``drop_extra_pre_encoded``,
  ``valid_out_len``) that a torch-free inference loop needs to replicate
  NeMo's ``pad_and_drop_preencoded`` streaming semantics, including the
  one-chunk-of-silence flush at end of audio.

torch's ONNX exporter cannot export NeMo's STFT, so this script does not
produce a mel-frontend graph; a torch-free inference loop runs the frontend
(``normalize`` NA) as hand-written numpy instead.

Findings from running this recipe (English 82 clips, FLEURS 40 clips/lang,
2026-09-17) are in ``server/.kb/model-evaluation.md``. Nemotron 3.5 is parked,
not packaged: see that document for why. This script is a reference for
reviving the evaluation, not part of any gated build.
"""

import json
import sys
import time
from pathlib import Path

import nemo.collections.asr as nemo_asr
import onnx
import onnxruntime as ort
import torch

nemo_path, out, right = sys.argv[1], Path(sys.argv[2]), int(sys.argv[3])
out.mkdir(parents=True, exist_ok=True)

model = nemo_asr.models.ASRModel.restore_from(nemo_path, map_location="cpu").eval()
model.encoder.set_default_att_context_size([56, right])
cfg = model.encoder.streaming_cfg
print("streaming_cfg:", cfg, flush=True)
print("preprocessor:", dict(model.cfg.preprocessor), flush=True)

t0 = time.time()
model.set_export_config({"cache_support": "True"})
model.export(str(out / "model.onnx"))
print(
    "exported",
    f"{time.time() - t0:.0f}s",
    sorted(p.name for p in out.iterdir() if p.suffix == ".onnx"),
    flush=True,
)


class PromptProjection(torch.nn.Module):
    """encoder frames (B, D, T) + one-hot language (B, P) -> conditioned
    frames (B, D, T), exactly PromptStreamingMixin._apply_prompt_to_encoded."""

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
print("prompt graph max abs diff vs torch:", float(abs(got - ref).max()), flush=True)

vocab = [*model.tokenizer.vocab, "<blk>"]
with (out / "vocab.txt").open("w", encoding="utf-8") as fh:
    for i, token in enumerate(vocab):
        fh.write(f"{token} {i}\n")
(out / "prompts.json").write_text(
    json.dumps(dict(model.cfg.model_defaults.prompt_dictionary), indent=1)
)
(out / "streaming.json").write_text(
    json.dumps(
        {k: (v if not hasattr(v, "tolist") else v.tolist()) for k, v in vars(cfg).items()},
        default=str,
        indent=1,
    )
)

for name in sorted(p.name for p in out.iterdir() if p.suffix == ".onnx"):
    m = onnx.load(str(out / name), load_external_data=False)
    print(
        name,
        "inputs:",
        [
            (i.name, [d.dim_value or d.dim_param for d in i.type.tensor_type.shape.dim])
            for i in m.graph.input
        ],
    )
    print(
        name,
        "outputs:",
        [
            (o.name, [d.dim_value or d.dim_param for d in o.type.tensor_type.shape.dim])
            for o in m.graph.output
        ],
    )
