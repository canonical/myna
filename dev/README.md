# `dev/` — developer scripts

Everything here is a tool for working *on* Myna. Nothing here is shipped.

## Benchmarking lives elsewhere

There is one benchmarking tool, `myna.benchmarker`
(`server/src/myna/benchmarker/`): corpus builders, the clip scorer, the snap
sweep, the aggregator and the environment guard. Run it in-tree with the
`make bench-*` targets, or pack it for external testers with `make build-bench`
(`myna-bench.pyz`). It has no dependency on this directory, which is what lets
the same code run from a checkout and from a tester's download.

```shell
make bench-check          # is this machine fit to benchmark on?
make bench-plan           # every row the sweep would produce; no root
make bench-run            # the sweep (sudo: installs and purges snaps)
make bench-aggregate      # re-print the table
```

Only the two configs stayed here:

| File | What it is |
| --- | --- |
| `bench.yaml` | The in-repo sweep: which snaps, which axes. Same format as the example below; its `files:` globs point into the `*-snap/` directories of this tree. |
| `bench.yaml.example` | The template testers edit, pointing at downloaded artefacts instead. Carries the annotated schema both files share. |

## What is here

| Script | What it does |
| --- | --- |
| `generate_fixtures.py` | Synthetic espeak fixture tier for the offline test suite. Not a WER corpus — the synthetic voice is out of distribution and scores misleadingly across architectures. |
| `fetch_audio8_model.py`, `fetch_funasr_model.py`, `fetch_sherpa_model.py`, `parakeet/fetch_parakeet_onnx.py` | Fetch and stage model weights into a snap directory. Driven by the `snap-*` make targets. |
| `parakeet/build_maxstack_encoder.py`, `parakeet/requantize_encoder.py`, `parakeet/collapse_probe.py`, `parakeet/build-maxstack.sh`, `parakeet/qsilu/` | Build and validate alternative Parakeet encoders. |
| `stage-qwen-c.sh`, `model-pin.sh`, `lint-packages.sh` | Snap staging and packaging checks. |
| `spread-build.sh`, `spread-image.sh` | Confined end-to-end (spread) harness. |
| `adapter_coverage.py`, `coverage_populations.py`, `coverage_lib.py`, `gjs_coverage.py`, `patch_cov.py`, `shexli_gate.py`, `vulture_allowlist.py` | Coverage reports and gates behind `make coverage` (and the shexli gate CI's `extension-review` job runs). |
| `exercise.sh`, `gated-tests.sh`, `transcribe.py`, `capabilities.py` | Manual drivers for a running server. |
| `i18n.sh`, `ibus-doctor.sh` | Translation templates, IBus diagnosis. |
