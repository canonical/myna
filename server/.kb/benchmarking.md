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
- `myna.testbed.metrics.normalize` casefolds and strips punctuation
  (NFKC + casefold, drop non-word/non-apostrophe chars, keep intra-word
  apostrophes) - matching NVIDIA's FLEURS-card convention of
  punctuation-and-case removal only, not Whisper's `EnglishTextNormalizer`/
  `BasicTextNormalizer` (decided 2026-09-17: fold apostrophes only, no style
  switch). Typographic apostrophes (U+2019, U+2018, U+02BC) fold to ASCII `'`
  before that, because FLEURS French references use U+2019 for elisions and
  an ASCII hypothesis otherwise splits "l'accident" into two words against
  them. `normalizer_version` is stamped on every row; `one_normalizer_version`
  refuses a file whose rows were scored under different versions, same as
  `one_corpus` does for `corpus_id`.
- Full FLEURS test set, Parakeet v3 fp32 CUDA, 2026-09-17 (de 862 / es 908 /
  fr 676 clips): the apostrophe fold moved fr 7.78 -> 5.46 WER (published
  5.15), de 5.16 -> 5.14 (published 5.04), es unchanged at 3.62 (published
  3.45) - all within bootstrap CI of NVIDIA's published numbers. The bundled
  40-clip FLEURS subset's bootstrap CI half-width is ~2-4 WER points, so a
  point estimate on it is not comparable to a published number; use the full
  test set for that comparison.

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
