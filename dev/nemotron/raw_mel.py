#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["onnx==1.23.2"]
# ///
"""Cut the per-feature normalisation off Parakeet's ``nemo128.onnx`` mel
graph, leaving the raw log-mel Nemotron wants (its preprocessor is
``normalize: NA``).

    dev/nemotron/raw_mel.py NEMO128.onnx OUT.onnx

The graph's log-mel is ``tmp_21`` (batch, 128, frames), just before the
masked mean/variance tail; that node's output becomes ``features``. Checked
on 2026-11-02 against NeMo's own preprocessor (dither 0): mean abs diff
0.0008, and int8 WER on the caption clips unchanged.
"""

import sys
from pathlib import Path

import onnx


def cut(src: Path, dst: Path) -> None:
    model = onnx.load(str(src))
    graph = model.graph
    last = next(i for i, node in enumerate(graph.node) if node.output[0] == "tmp_21")
    nodes = list(graph.node[: last + 1])
    nodes[-1].output[0] = "features"
    del graph.node[:]
    graph.node.extend(nodes)
    del graph.output[:]
    graph.output.append(
        onnx.helper.make_tensor_value_info(
            "features", onnx.TensorProto.FLOAT, ["batch_size", 128, "T"]
        )
    )
    used = {name for node in graph.node for name in node.input}
    kept = [init for init in graph.initializer if init.name in used]
    del graph.initializer[:]
    graph.initializer.extend(kept)
    onnx.checker.check_model(model)
    onnx.save(model, str(dst))


if __name__ == "__main__":
    cut(Path(sys.argv[1]), Path(sys.argv[2]))
