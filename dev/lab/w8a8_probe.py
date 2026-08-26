#!/usr/bin/env python3
"""Can RedHatAI/whisper-tiny-quantized.w8a8 ship as a Myna model component?

Three questions, answered with measurements rather than the model card:

1. **Does it run at all off vLLM?** The card documents one deployment path
   (vLLM >= 0.5.2, i.e. a CUDA/ROCm server stack). Myna's whisper snap is
   CTranslate2 on CPU. This loads the checkpoint through
   transformers + compressed-tensors on CPU and reports what happens.
2. **What does it weigh?** Compared against the FP16 CTranslate2 weights the
   snap ships today, and against a CTranslate2 INT8 conversion of the same
   base model - the quantization we can already do for free at load time.
3. **What is its accuracy on our corpus?** Scored with `myna.testbed.metrics`
   against the un-quantized `openai/whisper-tiny` run through the same
   transformers path, so the INT8 delta is measured here rather than
   transferred from the card's LibriSpeech numbers.

Lab tool: it deliberately bypasses the session contract and the snap. Run it
in a throwaway venv carrying torch + transformers + compressed-tensors +
ctranslate2 - none of which are Myna dependencies, and none of which this
concludes we should add.

    python dev/lab/w8a8_probe.py --manifest corpus/real/manifest-balanced.json
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
import traceback
import wave
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

from myna.testbed.metrics import character_error_rate, word_error_rate

W8A8_REPO = "RedHatAI/whisper-tiny-quantized.w8a8"
BASE_REPO = "openai/whisper-tiny"


def du_bytes(path: Path) -> int:
    return sum(f.stat().st_size for f in Path(path).rglob("*") if f.is_file())


def human(n: int) -> str:
    return f"{n / 1e6:.1f} MB"


def read_wav(path: Path):
    import numpy as np

    with wave.open(str(path), "rb") as w:
        pcm = w.readframes(w.getnframes())
    return np.frombuffer(pcm, dtype=np.int16).astype(np.float32) / 32768.0


def load_clips(manifest: Path, limit: int | None, exclude_categories: set[str]):
    """Load clips, dropping the excluded categories.

    ``long-form`` is excluded by default and must stay excluded here.
    ``WhisperForConditionalGeneration.generate`` decodes a single 30 s window
    unless the long-form path is engaged; the balanced tier's ~5 min chapter
    would then contribute 30 s of hypothesis against 5 min of reference and,
    being a third of the corpus by duration, would dominate every WER in the
    table. Long-form behaviour is a decoder question, and it is answered on
    the shipped CTranslate2 path by dev/lab/whisper_decode_sweep.py - not
    here, where the question is what the *weights* cost."""
    data = json.loads(manifest.read_text())
    clips = [c for c in data["clips"] if c.get("category") not in exclude_categories]
    if limit:
        clips = clips[:limit]
    return [(c, read_wav(manifest.parent / c["path"])) for c in clips]


def run_transformers(repo: str, clips, cache_dir: str | None, decompress: bool = False):
    """Decode every clip through transformers on CPU. Returns a result dict,
    or one carrying `error` when the checkpoint will not load/run here.

    ``decompress`` asks compressed-tensors to expand the INT8 weights back to
    float at load time. That is not a deployment path - it buys none of the
    speed or memory the quantization exists for - but it is the only way to
    score the *quantized weights* on CPU, so the accuracy question can be
    answered separately from the runtime question."""
    import torch
    from transformers import WhisperForConditionalGeneration, WhisperProcessor

    out: dict = {"repo": repo, "decompressed": decompress}
    try:
        t0 = time.perf_counter()
        processor = WhisperProcessor.from_pretrained(repo, cache_dir=cache_dir)
        kwargs: dict = {}
        if decompress:
            from transformers import CompressedTensorsConfig

            kwargs["quantization_config"] = CompressedTensorsConfig(
                run_compressed=False
            )
        model = WhisperForConditionalGeneration.from_pretrained(
            repo, cache_dir=cache_dir, dtype=torch.float32, **kwargs
        )
        model.eval()
        out["load_seconds"] = round(time.perf_counter() - t0, 2)
    except Exception as exc:  # noqa: BLE001 - the failure IS the finding
        out["error"] = f"{type(exc).__name__}: {exc}"
        out["traceback"] = traceback.format_exc()[-2000:]
        return out

    # What the loader actually left in memory: a real INT8 runtime keeps int8
    # tensors, a decompress-on-load path hands back floats and buys nothing.
    dtypes: dict[str, int] = {}
    for p in model.parameters():
        dtypes[str(p.dtype)] = dtypes.get(str(p.dtype), 0) + p.numel()
    out["param_dtypes"] = {
        k: v for k, v in sorted(dtypes.items(), key=lambda kv: -kv[1])
    }
    out["quant_config"] = str(getattr(model.config, "quantization_config", None))[:400]

    wer_edits = wer_ref = cer_edits = cer_ref = 0
    audio_seconds = decode_seconds = 0.0
    try:
        for clip, samples in clips:
            feats = processor(
                samples, sampling_rate=16_000, return_tensors="pt"
            ).input_features
            t0 = time.perf_counter()
            with torch.no_grad():
                ids = model.generate(
                    feats, language="en", task="transcribe", max_new_tokens=200
                )
            decode_seconds += time.perf_counter() - t0
            audio_seconds += clip["duration_seconds"]
            text = processor.batch_decode(ids, skip_special_tokens=True)[0].strip()

            w = word_error_rate(clip["text"], text)
            c = character_error_rate(clip["text"], text)
            wer_edits += w.substitutions + w.deletions + w.insertions
            wer_ref += w.reference_length
            cer_edits += c.substitutions + c.deletions + c.insertions
            cer_ref += c.reference_length
    except Exception as exc:  # noqa: BLE001
        out["error"] = f"decode failed: {type(exc).__name__}: {exc}"
        out["traceback"] = traceback.format_exc()[-2000:]
        return out

    out.update(
        clips=len(clips),
        wer=round(wer_edits / wer_ref, 5) if wer_ref else None,
        cer=round(cer_edits / cer_ref, 5) if cer_ref else None,
        audio_seconds=round(audio_seconds, 2),
        decode_seconds=round(decode_seconds, 2),
        rtf=round(decode_seconds / audio_seconds, 4) if audio_seconds else None,
    )
    return out


def convert_ct2(out_dir: Path, quantization: str, cache_dir: str | None) -> dict:
    """ct2-transformers-converter the base checkpoint, to size the INT8 weights
    we could ship without a special upstream artefact."""
    if out_dir.exists():
        return {
            "quantization": quantization,
            "dir": str(out_dir),
            "bytes": du_bytes(out_dir),
            "reused": True,
        }
    converter = Path(sys.executable).with_name("ct2-transformers-converter")
    cmd = [
        str(converter if converter.exists() else "ct2-transformers-converter"),
        "--model",
        BASE_REPO,
        "--output_dir",
        str(out_dir),
        "--quantization",
        quantization,
        "--copy_files",
        "tokenizer.json",
        "preprocessor_config.json",
    ]
    env = dict(os.environ)
    if cache_dir:
        env["HF_HOME"] = cache_dir
    # check=False on purpose: a failed conversion is a reportable row, not a
    # reason to lose the transformers runs already in the report.
    done = subprocess.run(cmd, capture_output=True, text=True, env=env, check=False)
    if done.returncode != 0:
        return {"quantization": quantization, "error": done.stderr[-1500:]}
    return {
        "quantization": quantization,
        "dir": str(out_dir),
        "bytes": du_bytes(out_dir),
    }


def write_report(report: dict, out: Path) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2))


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--manifest",
        type=Path,
        default=REPO_ROOT / "corpus/real/manifest-balanced.json",
    )
    p.add_argument("--limit", type=int, default=None)
    p.add_argument(
        "--exclude-categories",
        nargs="*",
        default=["long-form"],
        help="corpus categories to skip (see load_clips)",
    )
    p.add_argument(
        "--cache-dir", default=None, help="HF cache dir (defaults to HF_HOME)"
    )
    p.add_argument(
        "--ct2-dir",
        type=Path,
        default=None,
        help="where to write the CTranslate2 conversions",
    )
    p.add_argument("--out", type=Path, default=REPO_ROOT / "results/w8a8-probe.json")
    args = p.parse_args()

    clips = load_clips(args.manifest, args.limit, set(args.exclude_categories))
    print(
        f"{len(clips)} clips from {args.manifest} "
        f"(excluding {args.exclude_categories or 'nothing'})",
        file=sys.stderr,
        flush=True,
    )

    report: dict = {
        "manifest": str(args.manifest),
        "clips": len(clips),
        "excluded_categories": sorted(args.exclude_categories),
        "runs": {},
        "sizes": {},
    }

    shipped = REPO_ROOT / "whisper-snap/components/model-tiny-ct2"
    if shipped.exists():
        report["sizes"]["shipped-ct2-fp16"] = du_bytes(shipped)

    plan = (
        # As published: compressed INT8 weights, INT8 activations. The runtime
        # question - does it execute anywhere other than vLLM?
        ("w8a8-as-published", W8A8_REPO, False),
        # Same weights expanded to float. The accuracy question, isolated.
        ("w8a8-decompressed", W8A8_REPO, True),
        # The un-quantized reference, same decoder, same corpus.
        ("base-fp32", BASE_REPO, False),
    )
    for tag, repo, decompress in plan:
        print(f"--- transformers/{tag}", file=sys.stderr, flush=True)
        report["runs"][tag] = run_transformers(repo, clips, args.cache_dir, decompress)
        print(
            json.dumps(report["runs"][tag], indent=1)[:1200],
            file=sys.stderr,
            flush=True,
        )
        write_report(report, args.out)  # a later step must not lose this

    if args.ct2_dir:
        for q in ("int8", "float32"):
            print(f"--- ct2 convert {q}", file=sys.stderr, flush=True)
            report["sizes"][f"ct2-{q}"] = convert_ct2(
                args.ct2_dir / f"tiny-{q}", q, args.cache_dir
            )
            write_report(report, args.out)

    write_report(report, args.out)
    print(f"\nwrote {args.out}")
    for k, v in report["sizes"].items():
        print(f"  size {k}: {human(v) if isinstance(v, int) else v}")
    for tag, run in report["runs"].items():
        if "error" in run:
            print(f"  {tag}: FAILED - {run['error'][:200]}")
        else:
            print(
                f"  {tag}: WER {run['wer']:.4f} CER {run['cer']:.4f} RTF {run['rtf']:.3f} "
                f"dtypes {run['param_dtypes']}"
            )
    print(
        "\n(RTF here is a transformers-on-CPU reference decode, not the shipped "
        "CTranslate2 path - compare RTF only within this table.)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
