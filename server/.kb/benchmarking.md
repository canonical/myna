# Preface

Read this document when running, extending, or interpreting Myna benchmarks. Benchmark runs install and purge snaps and must not be executed on a machine whose Myna installation must be preserved.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

`myna.benchmarker` is the only benchmark implementation. `make bench-*` uses the same standalone `myna-bench.pyz` artifact distributed to external test machines, so local and remote measurements have the same semantics.

# Important

- Run `make bench-check` and `make bench-plan` before a sweep.
- `make bench-run` uses sudo, installs snap artifacts, and removes them with purge.
- Use real generated corpora for accuracy. Synthetic fixtures test plumbing and latency only.
- Compare rows only when their `corpus_id` values match.
- Treat `USABILITY_FAIL` as a product result, not a transient test failure.
- Explicit engine requests must fail rather than silently fall back.

# Architecture

The local workflow is:

```shell
make bench-check
make bench-plan
make bench-corpus
make bench-run-whisper
make bench-aggregate
```

For another machine, build `myna-bench.pyz` with `make build-bench`, copy it with the selected `.snap` and `.comp` artifacts and a configuration based on `dev/bench.yaml.example`, then run:

```shell
python3 myna-bench.pyz download-corpus --out ./corpus \
    --select balanced -n 80 --manifest-name manifest-balanced.json \
    --long-form-minutes 5
python3 myna-bench.pyz check --sweep
python3 myna-bench.pyz plan --config bench.yaml
sudo python3 myna-bench.pyz run --config bench.yaml
python3 myna-bench.pyz summarize --in results.jsonl --by-category
```

Merge returned submissions with:

```shell
make bench-merge SUBMISSIONS="incoming/a.jsonl incoming/b.jsonl"
```

Rows are identified by machine and label. Re-running a machine replaces its rows. Labels describe snap, engine, model, mode, and optional config; `provenance.settings` records the effective setting values.

WER and CER are micro-averaged. Speed is audio duration divided by decode time. Final latency measures end-of-audio to committed text; cold load measures session open to ready. Incomplete rows sort behind successful rows because partial metrics are not comparable.
