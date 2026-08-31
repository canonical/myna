#!/usr/bin/env python3
"""Stage the GigaAM-v3 e2e RNN-T ONNX model for the myna gigaam adapter.

Exports SberDevices' GigaAM-v3 ``v3_e2e_rnnt`` checkpoint (MIT) into the
ONNX layout the adapter consumes:

    encoder.onnx   log-mel features -> encoded frames
    decoder.onnx   prediction network (embedding + 1-layer LSTM, stateful)
    joint.onnx     fuses encoder frame + prediction -> vocab logits
    tokens.txt     one SentencePiece piece per line ('▁' = word boundary)

The export itself needs torch + the gigaam package, but only here at
component-build time — the adapter runs on ONNX Runtime alone. Run through
uv so the heavy export deps land in an ephemeral environment:

    uv run --with 'gigaam[torch]' --extra gigaam ./dev/fetch_gigaam_model.py \
        --target ./gigaam-snap/components/model-gigaam-onnx

Offline by contract (constitution V): the adapter never downloads at
session time; this script is the only network touch.

Also emits the mel golden reference for tests/test_gigaam_unit.py when
--emit-mel-reference is passed (numpy-computed chirp through the upstream
torchaudio preprocessor).
"""

import argparse
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", default="v3_e2e_rnnt", help="gigaam model name")
    parser.add_argument(
        "--target",
        default="./gigaam-snap/components/model-gigaam-onnx",
        help="output model directory (snap component layout)",
    )
    parser.add_argument(
        "--emit-mel-reference",
        action="store_true",
        help="also regenerate server/tests/data/gigaam_mel_golden.txt",
    )
    args = parser.parse_args()

    import torch

    import gigaam

    model = gigaam.load_model(args.model)
    out = Path(args.target)
    out.mkdir(parents=True, exist_ok=True)

    model.to_onnx(dir_path=str(out), dtype=torch.float32)
    for src, dst in (
        (f"{args.model}_encoder.onnx", "encoder.onnx"),
        (f"{args.model}_decoder.onnx", "decoder.onnx"),
        (f"{args.model}_joint.onnx", "joint.onnx"),
    ):
        src_path = out / src
        if src_path.exists():
            src_path.rename(out / dst)
            print(f"{src} -> {dst}")

    tokenizer = model.decoding.tokenizer.model
    (out / "tokens.txt").write_text(
        "\n".join(tokenizer.IdToPiece(i) for i in range(tokenizer.get_piece_size())) + "\n",
        encoding="utf-8",
    )
    print(f"tokens.txt: {tokenizer.get_piece_size()} pieces")

    if args.emit_mel_reference:
        import numpy as np

        n = 32000
        t = np.arange(n) / 16000.0
        # numpy-computed on purpose: the golden must come from the exact
        # chirp the unit test builds (torch's sin differs from numpy's at
        # these phases; see the test's docstring).
        chirp = (np.sin(2.0 * np.pi * (200.0 + 600.0 * t) * t) * 0.5).astype(np.float32)
        feats = model.preprocessor.featurizer(torch.from_numpy(chirp).unsqueeze(0))
        ref = Path(__file__).resolve().parent.parent / "server" / "tests" / "data" / "gigaam_mel_golden.txt"
        np.savetxt(ref, feats[0, :, :50].numpy(), fmt="%.8f")
        print(f"mel golden: {ref}")

    print("done:", out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
