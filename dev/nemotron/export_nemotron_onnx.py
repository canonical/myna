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
"""Export a cache-aware streaming Nemotron checkpoint to the ONNX graphs
Myna's ``nemotron`` adapter serves.

    dev/nemotron/export_nemotron_onnx.py {en,3.5} [--right-context N]

Fetches NVIDIA's checkpoint at a pinned revision, sha256-verified (a copy
already in the Hugging Face cache is used if its bytes match), and writes
``nemotron-<model>-rc<N>`` into the model cache (``$XDG_CACHE_HOME/myna/models``),
stamped with what it was made from:

- ``encoder-model.onnx`` and ``decoder_joint-model.onnx`` (fp32, weights in
  one ``.data`` file each), from NeMo's exporter with ``cache_support``, and
  their ``.int8.onnx`` twins (dynamic; the encoder's MatMul weights only).
- ``prompt-model.onnx``, for 3.5 only: NeMo's exporter drops
  ``PromptStreamingMixin._apply_prompt_to_encoded``.
- ``mel.onnx``: Parakeet's pinned ``nemo128.onnx`` without its normalisation
  (``raw_mel.py``); torch cannot export NeMo's STFT.
- ``vocab.txt``, ``config.json``: tokens (blank last), the streaming shape,
  the cache sizes, the preprocessor's settings and the language prompts.

The right context fixes the graph's chunk: 6 (0.56 s) ships, 13 (1.12 s)
scored the same and commits 0.4 s later. Exporting and quantising peak around
7 GB of memory.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Checkpoint:
    repo: str
    revision: str
    file: str
    sha256: str


# Keep in step with nemotron-snap/dev/download-models.sh; test_model_pins.py
# holds the two together.
MODELS = {
    "en": Checkpoint(
        repo="nvidia/nemotron-speech-streaming-en-0.6b",
        revision="7a9b763e6c5fb103da690219c049fac917aa50b1",
        file="nemotron-speech-streaming-en-0.6b.nemo",
        sha256="283638054c44f6794e74fe9af9048d78a6d9d6c058c12131856c7859a62ac9cd",
    ),
    "3.5": Checkpoint(
        repo="nvidia/nemotron-3.5-asr-streaming-0.6b",
        revision="ea30d66debe3740a08b573244286791d423d6b3e",
        file="nemotron-3.5-asr-streaming-0.6b.nemo",
        sha256="210214ed94039bf6bfbb9a047c7fa289628db75b103e2bf6381fa78285436a74",
    ),
}
RIGHT_CONTEXT = 6
# Bump when the graphs this script writes change for the same checkpoint, so a
# cache made by the old recipe is re-exported rather than staged.
RECIPE = 1
STAMP_FILE = "UPSTREAM_REVISION"

HERE = Path(__file__).resolve().parent


def stamp(model: str, right: int = RIGHT_CONTEXT) -> str:
    c = MODELS[model]
    return f"{c.repo}@{c.revision} rc {right} recipe {RECIPE}"


def cache_root() -> Path:
    return Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")) / "myna" / "models"


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as fh:
        for block in iter(lambda: fh.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch_checkpoint(c: Checkpoint, root: Path) -> Path:
    """The pinned .nemo: from the Hugging Face cache if its bytes match, else
    downloaded resumably and verified."""
    hub = Path(os.environ.get("HF_HUB_CACHE", Path.home() / ".cache" / "huggingface" / "hub"))
    cached = hub / f"models--{c.repo.replace('/', '--')}" / "snapshots" / c.revision / c.file
    if cached.exists() and _sha256(cached) == c.sha256:
        return cached
    path = root / "nemotron-checkpoints" / c.file
    if path.exists() and _sha256(path) == c.sha256:
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://huggingface.co/{c.repo}/resolve/{c.revision}/{c.file}"
    have = path.stat().st_size if path.exists() else 0
    req = urllib.request.Request(url, headers={"Range": f"bytes={have}-"} if have else {})
    with urllib.request.urlopen(req, timeout=60) as resp, path.open("ab") as out:
        while block := resp.read(1 << 20):
            out.write(block)
            print(f"\r{c.file}: {(have + out.tell()) / 1e6:.0f} MB", end="", file=sys.stderr)
    print(file=sys.stderr)
    if (digest := _sha256(path)) != c.sha256:
        raise RuntimeError(f"sha256 mismatch: {digest} != {c.sha256} ({url})")
    return path


def mel_graph(root: Path, dst: Path) -> None:
    """``mel.onnx`` from the murmure bundle Parakeet's CPU engine pins."""
    sys.path[:0] = [str(HERE), str(HERE.parent / "parakeet")]
    import fetch_parakeet_onnx
    import raw_mel

    src = fetch_parakeet_onnx.stage(root / "parakeet-tdt-0.6b-v3-int8") / "nemo128.onnx"
    raw_mel.cut(src, dst)


def export(checkpoint: Path, out: Path, right: int) -> None:
    import nemo.collections.asr as nemo_asr
    import onnx
    import onnxruntime as ort
    import torch
    from onnxruntime.quantization import QuantType, quantize_dynamic

    model = nemo_asr.models.ASRModel.restore_from(str(checkpoint), map_location="cpu").eval()
    left = model.encoder.att_context_size[0]
    model.encoder.set_default_att_context_size([left, right])
    cfg = model.encoder.streaming_cfg

    # NeMo's exporter scatters weights into one file per tensor; gather each
    # graph's into one beside it. The quantiser's scratch copies go to disk,
    # not a tmpfs /tmp: three of them are 7 GB.
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
        diff = float(abs(got - ref).max())
        if diff > 1e-3:
            raise RuntimeError(f"prompt graph differs from torch by {diff}")
        print("prompt graph max abs diff:", diff, flush=True)

    def plain(value: object) -> object:
        return value.tolist() if hasattr(value, "tolist") else value

    with (out / "vocab.txt").open("w", encoding="utf-8") as fh:
        for i, token in enumerate([*model.tokenizer.vocab, "<blk>"]):
            fh.write(f"{token} {i}\n")
    caches = model.encoder.get_initial_cache_state(batch_size=1)
    (out / "config.json").write_text(
        json.dumps(
            {
                "source": checkpoint.name,
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
                            for c in caches
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
        t0 = time.time()
        # The encoder's MatMuls only: quantising its 77 Conv layers as well
        # cost alestro 0.3 WER points and ran slower (0.241 core against
        # 0.205), measured 2026-11-02. The decoder_joint keeps every op.
        quantize_dynamic(
            out / f"{part}-model.onnx",
            out / f"{part}-model.int8.onnx",
            weight_type=QuantType.QInt8,
            op_types_to_quantize=["MatMul"] if part == "encoder" else None,
        )
        print("quantised", part, f"{time.time() - t0:.0f}s", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("model", choices=sorted(MODELS))
    parser.add_argument("--right-context", type=int, default=RIGHT_CONTEXT)
    args = parser.parse_args()

    root = cache_root()
    out = root / f"nemotron-{args.model}-rc{args.right_context}"
    want = stamp(args.model, args.right_context)
    marker = out / STAMP_FILE
    if marker.exists() and marker.read_text(encoding="utf-8").strip() == want:
        print(f"{out} is already {want}")
        return 0
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    checkpoint = fetch_checkpoint(MODELS[args.model], root)
    export(checkpoint, out, args.right_context)
    mel_graph(root, out / "mel.onnx")
    marker.write_text(f"{want}\n", encoding="utf-8")
    print(f"{out}: {want}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
