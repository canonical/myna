# qwen-snap — Qwen3-ASR inference snap (UbuSTT)

Adds Qwen3-ASR as a third model family — the snap that proves the
inference-snap architecture plugs in any model. **Ships the CPU pure-C engine.**

| engine | runtime | delivery | status |
|---|---|---|---|
| `cpu` | pure-C ([antirez/qwen-asr](https://github.com/antirez/qwen-asr), OpenBLAS) | `libqwen_asr.so` baked into the base; no pip deps | **shipped + verified** (FFI adapter, 2026-06-15) |

The CPU runtime is the parsimonious, no-GPU-required, multilingual option: a
~170 KB shared object driven via ctypes by `myna.testbed.qwen.QwenCAdapter`,
with **zero pip dependencies**. It is dictation-marginal on commodity CPUs (see
the plan's Qwen3 probe) but a clean diversity proof point.

A vLLM/GPU engine for the same family lives on the **`qwen3-vllm-gpu` branch**
(verified transcribing correctly outside confinement; strict-confinement parked
on Triton/`libcuda` — see `docs/qwen-vllm-confinement.md` there). It is kept off
this branch so the shipped snap carries no dead weight, and demonstrates that
runtimes are switchable per family via the existing engine mechanism — the cpu
engine here, a GPU engine there, selected by hardware.

## Build

```shell
./dev/prepare.sh                   # stage the myna wheel into wheels/
./dev/download-models.sh 0.6B      # fetch Qwen3-ASR-0.6B into components/ (add 1.7B when wanted)
snapcraft pack
```

Already have the weights locally? Reuse them without re-downloading (hardlinked
in; expects `$MYNA_MODEL_SRC/Qwen3-ASR-<size>/`):

```shell
MYNA_MODEL_SRC=/path/to/models ./dev/download-models.sh 0.6B
```

The `cpu` engine builds from source (gcc + `libopenblas-dev`) at pack time. No
network/GPU components — `snapcraft pack` produces `qwen_*.snap` +
`qwen+model-0-6b.comp`.

Model IDs are `0-6b` / `1-7b` (component names can't contain dots): e.g.
`qwen use-model 0-6b`.

## Adding the 1.7B model

This build ships only `0-6b` (so it never declares a component with no weights).
To also ship `1-7b`:

1. `./dev/download-models.sh 1.7B` (or `MYNA_MODEL_SRC=… ./dev/download-models.sh 1.7B`)
2. `snap/snapcraft.yaml`: add `"Qwen3-ASR-1.7B/*": (component/model-1-7b)/` to the
   `model-components` `organize`, and a `model-1-7b:` entry under `components:`.
3. `engines/cpu/engine.yaml`: add `- 1-7b` to `model.options`.
4. Add `models/1-7b/model.yaml` mirroring `models/0-6b/` (id/name `1-7b`,
   `components: [model-1-7b]`, `MODEL_DIR=$SNAP_COMPONENTS/model-1-7b`).

## Picking the runtime

Only the `cpu` engine ships on this branch, so `modelctl` auto-selects it
everywhere (incl. GPU boxes). Once the GPU engine lands (the `qwen3-vllm-gpu`
branch), selection is by hardware (GPU > CPU); a user on a GPU box who wanted
the lean C runtime anyway would need to *pin* an engine against auto-selection —
see the plan's T10b notes (engine pinning, shared surface with the T29/T30
residency toggles); no such override is wired yet.

## Layout

Mirrors `whisper-snap` / `nemotron-snap`: `engines/<name>/{engine.yaml,server}`,
`runtimes/<name>/runtime.yaml`, `models/<id>/model.yaml`, `scripts/server.sh`,
install/post-refresh hooks, weights as components (gitignored, fetched by
`dev/download-models.sh`).
