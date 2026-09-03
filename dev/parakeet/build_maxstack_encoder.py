#!/usr/bin/env python3
"""Build the maxstack encoder into a staged Parakeet model dir (perf, phase 2).

    cd server && uv run --extra parakeet python ../dev/parakeet/build_maxstack_encoder.py \
        --model-dir ~/.cache/myna/models/parakeet-tdt-0.6b-v3-int8

Deterministic post-processing of the pinned murmure bundle, ratified
2026-08-31. The whole graph pipeline lives in this file:

  1. requantize 10 of the 11 fp32 feed_forward*/linear2 MatMuls to int8
     (`/layers.13/feed_forward1/linear2/MatMul` is excluded: it alone
     reproduces the WER damage a blanket attempt at all 11 caused);
  2. fuse every closed SiLU island onto the myna.QSiLU/QSiLUSmooth custom
     ops (qsilu/ next to this script — the requant closes 10 more islands,
     47+24);
  3. pre-propagate DequantizeLinear below layout ops so onnxruntime's
     load-time pass has nothing to clone;
  4. re-derive value_info so ORT's SkipLayerNormalization fusion arms.

Measured on the reference machine: encode 184.5 -> 159.9 ms (-13.3%),
whole-utterance -11.7%, WER within churn. Output lands in the model dir as
``encoder-model.int8.maxstack.onnx`` plus a copy of ``libqsilu.so`` (build
it first: qsilu/build.sh); the adapter picks both up automatically
(``myna.testbed.parakeet.encoder_variant``) and falls back to the base
encoder when either is absent.

Needs the real calibration corpus (dev/fetch_english_corpus.py) for step 1.
Calibration honours a clip-length cap (see requantize_encoder.py); run
under a memory cap the first time on a new machine, per that script's
docstring.
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import sys
import tempfile
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

REPO_ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO_ROOT / "server" / "src"))

PIPELINE_VERSION = 1
# T13's one bad node: quantizing it measurably damages WER on real clips
# (sentinel bisection, T13 result.md). One documented negative on file; a
# second clean negative closes it permanently per the T13 SPEC.
EXCLUDED_NODE = "/layers.13/feed_forward1/linear2/MatMul"
STAMP_FILE = "MAXSTACK_REVISION"

# --- step 2: SiLU island fusion (T11) ---------------------------------------

# Ops an island may pass through and still close. Add/Div are deliberately
# absent: every Add in this graph is either residual-stream (escapes) or
# attention pos-bias (escapes into fp32 MatMul), and quantizing the residual
# path risks error accumulation across all 24 layers for no plumbing win.
ISLAND_OPS = frozenset({"Sigmoid", "Mul", "Relu"})


def find_closed_islands(graph) -> list[list]:
    """DequantizeLinear-rooted elementwise chains whose every path ends in a
    QuantizeLinear. Anything that escapes into LayerNorm, an fp32 MatMul, the
    residual stream or a graph output is load-bearing, not plumbing, and is
    not returned. One node list per island."""
    consumers = defaultdict(list)
    for n in graph.node:
        for i in n.input:
            consumers[i].append(n)

    islands = []
    for dq in graph.node:
        if dq.op_type != "DequantizeLinear":
            continue
        members, closed = [], True
        frontier = list(consumers.get(dq.output[0], []))
        seen: set[int] = set()
        while frontier:
            node = frontier.pop()
            if id(node) in seen:
                continue
            seen.add(id(node))
            if node.op_type == "QuantizeLinear":
                continue
            if node.op_type in ISLAND_OPS:
                members.append(node)
                for out in node.output:
                    frontier.extend(consumers.get(out, []))
            else:
                closed = False
        if members and closed:
            islands.append(members)
    return islands


def fuse_silu_islands(model) -> dict:
    """Replace each closed SiLU island with one myna custom-op node:

      FFN:  DQ -> Sigmoid -> Mul -> Mul(smooth) -> Q   =>  myna.QSiLUSmooth
      conv: DQ -> Sigmoid -> Mul                 -> Q   =>  myna.QSiLU

    The kernels (qsilu/silu_qop.cc) do the same fp32 math with the same
    single exit rounding, so this is a pure fusion: no calibration, no new
    quantization decisions — scales/zero-points are wired straight from the
    island's own DQ and Q. Relu islands (2 nodes, ~0.05 ms) are left alone.
    An all-QLinear version of this rewrite was tried and rejected: it costs
    an extra rounding step per island and measurably hurt WER.
    """
    import onnx

    g = model.graph
    inits = {i.name for i in g.initializer}
    consumers = defaultdict(list)
    producer = {}
    for n in g.node:
        for i in n.input:
            consumers[i].append(n)
        for o in n.output:
            producer[o] = n

    fused = {"QSiLUSmooth": 0, "QSiLU": 0, "skipped": 0}
    dead: set[int] = set()
    new_nodes = []
    for island in find_closed_islands(g):
        ops = sorted(n.op_type for n in island)
        if ops == ["Mul", "Mul", "Sigmoid"]:
            kind = "QSiLUSmooth"
        elif ops == ["Mul", "Sigmoid"]:
            kind = "QSiLU"
        else:
            fused["skipped"] += 1
            continue

        sigmoid = next(n for n in island if n.op_type == "Sigmoid")
        dq = producer[sigmoid.input[0]]
        assert dq.op_type == "DequantizeLinear", dq.op_type
        muls = [n for n in island if n.op_type == "Mul"]
        smooth_name = None
        if kind == "QSiLUSmooth":
            smooth_muls = [m for m in muls if any(i in inits for i in m.input)]
            act_muls = [m for m in muls if m not in smooth_muls]
            if len(smooth_muls) != 1 or len(act_muls) != 1:
                fused["skipped"] += 1
                continue
            smooth_name = next(i for i in smooth_muls[0].input if i in inits)
            last = smooth_muls[0]
        else:
            if len(muls) != 1:
                fused["skipped"] += 1
                continue
            last = muls[0]
        exit_qs = consumers[last.output[0]]
        if len(exit_qs) != 1 or exit_qs[0].op_type != "QuantizeLinear":
            fused["skipped"] += 1
            continue
        q = exit_qs[0]

        # Safety: every island-internal tensor must stay inside the island.
        island_ids = {id(n) for n in island}
        internal_ok = True
        for n in island:
            for o in n.output:
                for c in consumers[o]:
                    if id(c) not in island_ids and c is not q:
                        internal_ok = False
        # The DQ's fp32 output must feed only island members.
        for c in consumers[dq.output[0]]:
            if id(c) not in island_ids:
                internal_ok = False
        if not internal_ok:
            fused["skipped"] += 1
            continue

        inputs = [dq.input[0], dq.input[1], dq.input[2]]
        if smooth_name is not None:
            inputs.append(smooth_name)
        inputs += [q.input[1], q.input[2]]
        new_nodes.append(
            onnx.helper.make_node(
                kind,
                inputs=inputs,
                outputs=[q.output[0]],
                name=f"{sigmoid.name}_fused",
                domain="myna",
            )
        )
        dead.update({id(dq), id(q)} | island_ids)
        fused[kind] += 1

    keep = [n for n in g.node if id(n) not in dead]
    del g.node[:]
    g.node.extend(keep)
    g.node.extend(new_nodes)

    _toposort(model)  # new nodes were appended out of order
    op = model.opset_import.add()
    op.domain = "myna"
    op.version = 1
    return fused


def _toposort(model) -> None:
    g = model.graph
    produced = {i.name for i in g.initializer} | {i.name for i in g.input}
    ordered, pending = [], list(g.node)
    while pending:
        progress, rest = [], []
        for n in pending:
            if all(i in produced or i == "" for i in n.input):
                progress.append(n)
                produced.update(n.output)
            else:
                rest.append(n)
        if not progress:
            raise SystemExit("toposort stuck: cycle or dangling input")
        ordered.extend(progress)
        pending = rest
    del g.node[:]
    g.node.extend(ordered)


# --- step 3: DequantizeLinear pre-propagation --------------------------------

LAYOUT_OPS = {"Reshape", "Transpose"}


def prepropagate_qdq(model) -> dict:
    """Push each DequantizeLinear below its pure-layout consumers at export.

    At session load, onnxruntime's QDQ propagation does the same push (good:
    Reshape/Transpose then run on int8) but clones one DQ per consumer
    branch with no CSE afterward (bad): the shipped export's 281 DQs execute
    as 401, including 24 whose only consumer is a Shape node. Doing the push
    here, once, with exactly one DQ per landing point and Shape consumers
    retargeted to the int8 tensor (shape is dtype-independent) leaves the
    load-time optimizer nothing to move and nothing to clone. Exact by
    construction: DequantizeLinear is elementwise (per-tensor scale,
    asserted), so it commutes with layout ops.
    """
    g = model.graph
    inits = {i.name: i for i in g.initializer}
    stats = {"shape_retargeted": 0, "layout_hops": 0}
    stale_value_info: set[str] = set()

    changed = True
    while changed:
        changed = False
        consumers = defaultdict(list)
        for n in g.node:
            for i in n.input:
                consumers[i].append(n)
        graph_outputs = {o.name for o in g.output}

        for dq in list(g.node):
            if dq.op_type != "DequantizeLinear":
                continue
            scale = inits.get(dq.input[1])
            if scale is None or len([d for d in scale.dims if d > 1]) > 0:
                continue
            if dq.output[0] in graph_outputs:
                continue
            users = consumers[dq.output[0]]
            if len(users) != 1 or users[0].op_type not in LAYOUT_OPS:
                continue
            layout = users[0]
            int8_in = dq.input[0]
            fp32_out = dq.output[0]
            layout_out = layout.output[0]
            if layout_out in graph_outputs:
                continue
            # Swap: the layout op consumes the int8 tensor, the DQ its output.
            for i, inp in enumerate(layout.input):
                if inp == fp32_out:
                    layout.input[i] = int8_in
            dq.input[0] = layout_out
            dq.output[0] = fp32_out
            for c in consumers[layout_out]:
                if c is dq:
                    continue
                for i, inp in enumerate(c.input):
                    if inp == layout_out:
                        c.input[i] = fp32_out
            stale_value_info.update({fp32_out, layout_out})
            stats["layout_hops"] += 1
            changed = True
            break  # consumer map is stale now; rescan

    # Shape consumers of a DQ output read the int8 tensor instead.
    consumers = defaultdict(list)
    for n in g.node:
        for i in n.input:
            consumers[i].append(n)
    for dq in g.node:
        if dq.op_type != "DequantizeLinear":
            continue
        for c in consumers[dq.output[0]]:
            if c.op_type == "Shape":
                for i, inp in enumerate(c.input):
                    if inp == dq.output[0]:
                        c.input[i] = dq.input[0]
                        stats["shape_retargeted"] += 1

    used = {o.name for o in g.output}
    for n in g.node:
        used.update(n.input)
    keep = [
        n for n in g.node if n.op_type != "DequantizeLinear" or any(o in used for o in n.output)
    ]
    stats["dead_dq_dropped"] = len(g.node) - len(keep)
    del g.node[:]
    g.node.extend(keep)

    # These tensors changed position in the graph, so any recorded shapes for
    # them are wrong; drop the entries rather than confuse the runtime.
    if stale_value_info:
        kept_vi = [vi for vi in g.value_info if vi.name not in stale_value_info]
        del g.value_info[:]
        g.value_info.extend(kept_vi)
    _toposort(model)
    return stats


# --- step 4: shape metadata ---------------------------------------------------


def graft_shapes(model) -> None:
    """Re-derive full value_info despite the custom ops.

    onnx shape inference stops dead at an op it has no function for, which
    starves everything downstream of the myna ops - and ORT's
    SkipLayerNormalization fusion (24 sites, ~2.7 ms) silently disarms
    without proven shapes. Both custom ops are shape- and dtype-preserving
    (uint8 in, uint8 out, same dims), so infer on a metadata twin with each
    myna node swapped for Identity and graft the resulting value_info back.
    """
    import onnx
    from onnx import helper, shape_inference

    twin = onnx.ModelProto()
    twin.CopyFrom(model)
    for i, n in enumerate(list(twin.graph.node)):
        if n.domain == "myna":
            repl = helper.make_node(
                "Identity", [n.input[0]], [n.output[0]], name=n.name + "_shapetwin"
            )
            twin.graph.node[i].CopyFrom(repl)
    twin = shape_inference.infer_shapes(twin, data_prop=False)
    del model.graph.value_info[:]
    model.graph.value_info.extend(
        v for v in twin.graph.value_info if len(v.type.tensor_type.shape.dim) > 0
    )


# --- the pipeline -------------------------------------------------------------


def build(model_dir: Path, calib_glob: str, calib_n: int, qsilu_lib: Path) -> None:
    import onnx
    from requantize_encoder import _find_fp32_linear2_nodes, requantize

    from myna.testbed.corpus import digest_files

    if not qsilu_lib.exists():
        raise SystemExit(f"{qsilu_lib} missing — build it: dev/parakeet/qsilu/build.sh")

    encoder = model_dir / "encoder-model.int8.onnx"
    base = onnx.load(str(encoder), load_external_data=False)
    nodes = [n for n in _find_fp32_linear2_nodes(base) if n != EXCLUDED_NODE]
    if len(nodes) != 10:
        raise SystemExit(
            f"expected 10 quantizable fp32 linear2 nodes after excluding "
            f"{EXCLUDED_NODE}, found {len(nodes)} — the upstream export changed; "
            "re-run T13's blame assignment before trusting this pipeline"
        )

    out = model_dir / "encoder-model.int8.maxstack.onnx"
    with tempfile.TemporaryDirectory(dir=model_dir) as tmp:
        step1 = Path(tmp) / "step1.onnx"
        print("== step 1/4: requantize 10 FFN down-projections ==")
        calibration = requantize(str(model_dir), step1, calib_glob, calib_n, nodes=nodes)[
            "calibration_clips"
        ]

        model = onnx.load(str(step1), load_external_data=True)
        print("== step 2/4: fuse SiLU islands ==")
        stats = fuse_silu_islands(model)
        print(stats)
        if stats["QSiLUSmooth"] + stats["QSiLU"] < 70:
            raise SystemExit(f"fused only {stats} islands — expected 47+24; aborting")

        print("== step 3/4: pre-propagate DequantizeLinear ==")
        print(prepropagate_qdq(model))

        print("== step 4/4: re-derive shapes ==")
        graft_shapes(model)
        onnx.save(model, str(out))

    shutil.copy2(qsilu_lib, model_dir / qsilu_lib.name)
    lib_sha = hashlib.sha256((model_dir / qsilu_lib.name).read_bytes()).hexdigest()
    upstream = (model_dir / "UPSTREAM_REVISION").read_text(encoding="utf-8").strip()
    (model_dir / STAMP_FILE).write_text(
        f"maxstack {PIPELINE_VERSION}\nbase: {upstream}\nexcluded: {EXCLUDED_NODE}\n"
        f"libqsilu sha256: {lib_sha}\n"
        f"calibration: {digest_files(calibration)} ({len(calibration)} clips)\n",
        encoding="utf-8",
    )
    print(f"\nstaged {out} ({out.stat().st_size / 1e6:.1f} MB) + {qsilu_lib.name}")

    # Smoke: the adapter must pick the variant up and decode real audio.
    import glob as _glob
    import wave

    import numpy as np

    from myna.testbed.parakeet import _ParakeetOnnx, encoder_variant

    path, lib = encoder_variant(str(model_dir))
    assert path == str(out) and lib, "adapter did not select the maxstack variant"
    clip = sorted(_glob.glob(calib_glob))[0]
    with wave.open(clip, "rb") as w:
        samples = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)
    tokens, _ = _ParakeetOnnx(str(model_dir)).transcribe(samples.astype(np.float32) / 32768.0)
    if not tokens:
        raise SystemExit("smoke transcription returned no tokens")
    print(f"smoke: {len(tokens)} tokens from {Path(clip).name} — ok")


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--model-dir", type=Path, required=True)
    ap.add_argument("--calib-glob", default=str(REPO_ROOT / "corpus" / "real" / "audio" / "*.wav"))
    ap.add_argument("--calib-n", type=int, default=16)
    ap.add_argument(
        "--qsilu-lib", type=Path, default=Path(__file__).resolve().parent / "qsilu" / "libqsilu.so"
    )
    args = ap.parse_args()
    build(args.model_dir.expanduser(), args.calib_glob, args.calib_n, args.qsilu_lib)


if __name__ == "__main__":
    main()
