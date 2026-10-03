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
- Each row also carries a secondary score, `wer_whisper_norm`/
  `cer_whisper_norm` with their edit and reference counts, under Whisper's
  normalisers (`EnglishTextNormalizer` for `en*` clips, `BasicTextNormalizer`
  otherwise), so English numbers compare with the Open ASR Leaderboard. The
  normalisers are vendored in `myna.testbed.whisper_normalizers`, pinned to
  openai/whisper v20250625 and stamped as `secondary_normalizer_version`;
  bump both together. It never replaces the primary score. `summarize` shows
  it as `WERw%`/`CERw%`, in the table, the `--ci` intervals and `compare`,
  blank for a cell with any row scored before it existed; mixed secondary
  versions are refused like primary ones.
- Sanity check, 2026-09-30, full LibriSpeech test-clean (2620 clips), the
  myna-parakeet rev 2 int8 model in batch on a laptop CPU, intervals from
  `summarize --ci`: WERw 2.08 [1.92, 2.25] against NVIDIA's published 1.93
  for Parakeet TDT 0.6B v3 (fp32 NeMo; ours is the int8 SmoothQuant encoder,
  see `parakeet-snap/NOTICE`); ours reads 2.29 [2.12, 2.46] on the same rows.
- Full FLEURS test set, Parakeet v3 fp32 CUDA, 2026-09-17 (de 862 / es 908 /
  fr 676 clips): the apostrophe fold moved fr 7.78 -> 5.46 WER (published
  5.15), de 5.16 -> 5.14 (published 5.04), es unchanged at 3.62 (published
  3.45) - all within bootstrap CI of NVIDIA's published numbers. The bundled
  40-clip FLEURS subset's bootstrap CI half-width is ~2-4 WER points, so a
  point estimate on it is not comparable to a published number; use the full
  test set for that comparison.
- Those 2026-09-17 intervals were computed outside the tool, and the per-clip
  rows behind them were not kept, so they cannot be re-derived; treat them as
  indicative until a full FLEURS run is summarized with `--ci`.
- `summarize` prints 95% percentile-bootstrap intervals (10000 resamples,
  seed 0) for WER, CER, RTFx and median/p95 finalize latency. The clip is the
  resampling unit: a clip's repeats are drawn together, because they share
  audio and are not independent. Intervals need numpy on the host
  (`python3-numpy`); `--no-ci` skips them, and nothing else in the pyz needs it.
- RTFx is total audio over total processing seconds (Open ASR Leaderboard,
  batch size 1); `speed` is 1 / median per-clip RTF. Realtime-paced rows are
  left out of both, since their decode time is the pace.
- A p95 from fewer than 60 timed clips and a p99 from fewer than 300 print
  `n too small`, never a number. The floor counts clips, not rows: every
  repeat's latency is pooled into the percentile, but repeats of the same
  audio are correlated, so 20 clips x 3 repeats would still rest a p95 on the
  slowest one or two clips. The bootstrap applies the same floor per draw.
- `rep CV%` is the median within-clip coefficient of variation of finalize
  latency across repeats: a high value means the machine, not the model, is
  setting the timing.
- `compare A B` (`label` or `label@machine`) is a paired bootstrap over the
  clips both rows measured, same corpus and normalizer version: delta WER,
  CER and median finalize latency (A minus B), 95% interval, and a two-sided
  p-value from the re-centred replicates. The latency delta uses only clips
  timed on both sides (a starved realtime row or failed finalize drops a
  clip), so both medians of a draw pool the same clips. Compare two systems this way, not
  by eyeballing two table rows whose intervals overlap.

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

A number meant for a paper or a leaderboard comparison comes from a whole published test split, never a subset tier. `download-corpus --preset` builds one (`librispeech-test-clean`, `librispeech-test-other`, `fleurs-test:<locale>`; `corpus_publication`): every utterance, no noise or long-form variants, subset flags refused. Archives cache under the shared `~/.cache/myna/corpus-src`; the manifest names preset, dataset, split, licence, source URL and each archive's sha256. FLEURS is fetched at a pinned revision and scored against `raw_transcription`. A preset over a corpus that still verifies is a no-op; over a different corpus, refused. Sweep with `dev/bench-publication.yaml`, once per corpus with `--manifest` and its own `--out`: a results file is scored against one corpus id, and rows from different ids never compare. Sizes and costs are in that file's header. AMI, Earnings-22 and VoxPopuli wait on a licence decision; TED-LIUM 3 is CC-BY-NC-ND and excluded.

`--preset fleurs-smoke` is the one non-publication preset: the first 10 test clips by filename of each of en, de, fr, es, it, ru, zh, ja and ko in one manifest, category = locale. It feeds the nightly lab gate (`tests/testflinger/`), which downloads it as the `fleurs-smoke-v1` release asset pinned by sha256, and `summarize --gate <yaml>` fails that run on a per-snap, per-language ceiling (CER for zh/ja/ko/yue, WER otherwise), on a listed language nothing scored, or on a row that did not finish. A target may install from the store (`channel:` and `components:`, snap-downloaded and acked, so signed, not `--dangerous`) and restrict itself to `languages:`.

Merge returned submissions with:

```shell
make bench-merge SUBMISSIONS="incoming/a.jsonl incoming/b.jsonl"
```

Rows are identified by machine and label. Re-running a machine replaces its rows. Labels describe snap, engine, model, mode, and optional config; `provenance.settings` records the effective setting values.

Every row (clip, status and `*-resources.jsonl`) and the machine header carry `schema_version` (absent means 1, the unstamped schema); readers treat a missing field as unknown, never as an error. From schema 2 each sweep row's `provenance` also names the engine read back, the installed artifacts (file sha3-384 and size, snap version and revision, installed components), the harness (the `dev/version.sh` string `build-bench` bakes into the pyz, and the pyz's sha256), the OS state (kernel cmdline, microcode, governor, boost, SMT, snapd) and every NVIDIA GPU (driver, CUDA driver version, persistence, clocks, ECC). `served_runtime` is the server's own report of its inference libraries and execution provider, from capabilities re-read after the clips ran: the server names a library version only once a model load has imported it. Rows repeat all of this because `merge` keeps rows and drops headers.

`run` and `bench` also write `<out>-events.jsonl.gz`: one line per clip run, keyed like its row by (label, clip, repeat, phase), holding every server event and the audio-feed schedule (chunk send time, audio position reached), all timed from the first chunk sent; `audio_start` converts back to the row's session-open origin. It exists so a latency metric invented later is computed from data on disk, not a rerun; `summarize` and `merge` ignore it and `_events.load_events` reads it. About 195 KB per 83-clip balanced-corpus cell (streaming parakeet, 2026-09-29), so it is always on.

`run` samples each cell at 1 Hz from a separate process niced by 10 (`_telemetry`; `--no-resources` turns it off) and appends the trace to `<out>-resources.jsonl` as rows of kind `sample` (schema 3): per-core CPU frequency, package temperature (k10temp Tdie/Tctl, coretemp package, else `x86_pkg_temp`), cumulative RAPL package energy, the served process tree's RSS and VRAM, and per NVIDIA GPU its SM/memory clock, temperature, power, enforced power limit, thermal margin (`temperature.gpu.tlimit`, probed: older drivers reject it and it is left out), utilisation and clock-event (throttle) mask, from one long-lived `nvidia-smi -lms` per query. The cell's verdict row (kind `cell`, written last so readers that keep the last row per label still find it) holds peaks, energy, J per audio-second, max temperatures, `throttled` and `telemetry_error` (the sampler's own reason for an incomplete trace: died early, failed on stop, wrote nothing; also printed as a warning). Energy is the RAPL delta plus the trapezoid of every GPU's power, `gpu_energy_j_by_index` per GPU. It spans the whole cell - cold sample, warmup and measured passes - and divides by all audio fed in it; a GPU whose first nvidia-smi line lands after the first tick has its first reading held back to that tick (about 1 s, 4% of a short cell). Unknown stays null, never 0 J: a GPU without a power reading in every sample has null energy, and `energy_j` is null unless RAPL and every GPU in the trace were read, so a J/audio-s never silently drops a device; `cpu_energy_j` and `gpu_energy_j` still show the part that was read. RAPL's `energy_uj` is root-only, which `run` is. `throttled.gpu` counts a clock-event bit only in a busy sample (nonzero utilisation) and only when that reading bears it out: `sw_power_cap` needs power at 90% of the enforced limit, `sw_thermal` a thermal margin of 5 C or less; the `hw_*` bits count as reported. A laptop GPU reports `sw_power_cap | sw_thermal` at rest and in warm-up samples far below either limit (zephyrus: 20.5 W of 100 W at 52% util, 41 C from slowdown), so the raw bits would flag every cell. A power or thermal bit with its limit unread leaves `throttled.gpu` null and is named in `gpu_throttle_unverified`, which is what traces from before the limit columns read as. `throttled.cpu` exists only where Intel's `package_throttle_count` does, else null. The sampler costs about 0.9% of one core (zephyrus, 60 s, 2026-09-30). A zephyrus parakeet fp32 GPU cell reads 0.92 J/audio-s batch and 3.99 streaming at max pace, neither throttled; its enforced limit moved between 104 and 113 W within a cell (Dynamic Boost), which is why the limit is read per sample (2026-09-30).

Each cell runs `warmup_clips` first, then `repeats` full passes over the clips (config keys, global with per-target overrides; defaults 1 and 0, so a tester's run is one pass in manifest order). Rows carry `repeat` (0..N-1) and `phase` (`cold`, `warmup` or `measured`); `summarize` drops warmup rows and dedups by (label, clip, repeat, phase), and a row without the fields reads as repeat 0 in the phase its `cold` flag names. With more than one repeat, each pass is shuffled by `random.Random(f"{seed}/{repeat}")` (SHA-512 string seeding, so the order survives a new interpreter) and `provenance.schedule` records repeats, warmup and seed: drift then spreads across clips instead of biasing one. `sweep_budget_seconds` is per pass, so a cell's deadline is budget x repeats; warmup runs outside it, like the cold sample. A rerun with `--keep-results` and fewer repeats leaves the earlier run's higher repeats in the file, so rerun without it when the schedule changes.

`pace: [max, realtime]` (global, per-target override, default `[max]`) adds a real-time-paced cell to every streaming cell; batch always runs at `max`. `max` feeds as fast as the socket accepts, so streaming finalize latency there is not what a dictating user sees; `realtime` hands chunk k over at `origin + audio_end(k)` on a monotonic clock (`testbed.sources.paced`), catching up at once when behind, like a buffered microphone. Its label ends `@realtime`; `max` is unmarked so earlier rows still compare. Rows carry `pace`, `pace_lag` (worst delay behind the capture clock, from the first chunk) and `pace_starved` (lag over one chunk); `summarize` keeps a starved row's WER but drops its finalize latency. A realtime pass cannot beat its audio, so its budget is (budget + warm-pass audio) x repeats, and `plan` prices realtime cells from the manifest's durations.

`export --parquet <dir> --in results.jsonl` writes each cell (one label on one machine, every repeat, cold sample included) as an ai-inference-benchmark (AIB) unit, so AIB ingests our numbers through files, not imports: `<unit>.parquet` (one summary row), `<unit>.samples.parquet` (one row per clip run, errored runs as `ok = false`, warmup left out; a rerun's error never displaces an earlier success, which stays the scored row as in `summarize`, and a (clip, repeat, phase) whose last run errored after a success exports both) and `<unit>.load.parquet` (the cell's last telemetry trace). The unit id is AIB's recipe (16 hex digits of the sha256 of sorted-key JSON) over the label, installed artifacts, engine read back, served models, mode, pace, settings, `corpus_id`, both normaliser versions, the warm schedule and the digest of the environment (`machine` plus the provenance keys `cpu`, `ram_gb`, `gpu`, `gpu_vram_gb`, `provision`, `hardware`, `harness`, `os`, `gpus`). Unlike AIB's, the environment is part of the id: a number from another box or harness build is another unit. A cell whose rows disagree on any of it (a partial rerun under `--keep-results`) is refused. `campaign_id` is AIB's too, the hash of the sorted unit ids in the file. pyarrow is an optional extra (`parquet`): it is ~150 MB installed and ABI-specific, so the pyz does not carry it and `export` says how to get it when it is missing: `pip install pyarrow` in a venv and run the pyz with that python, or `python3-pyarrow` on Ubuntu 26.04+ (it is not packaged for noble or jammy). `--no-ci` also drops the numpy dependency, leaving the interval columns, `ci_resamples` and `ci_seed` null.

The Parquet files carry their own `schema_version` (`_parquet.EXPORT_SCHEMA_VERSION`, now 1), independent of the JSONL one, which rides along as `results_schema_version`; bump it when a column changes meaning or type. Against AIB's `src/inference_benchmark/results/schema.py` (261c485):

- Summary (`SUMMARY_SCHEMA`, lines 14-59): `schema_version` (16), `unit_id` (17), `campaign_id` (18), `env_*` (29-35), `started_at`/`ended_at` (36-37, first session open to last terminal event), `duration_s` (38), `requests_total`/`_ok`/`_error` (44-46, clip runs; errors are backend errors), `ram_peak_bytes`/`ram_mean_bytes` (47-48, served process tree RSS), `vram_peak_bytes`/`vram_mean_bytes` (49-50), `gpu_load_mean_pct` (52, every GPU) and `energy_joules` (53, our `energy_j`, null unless RAPL and every GPU read) keep AIB's names. `status` (19) maps ok -> `completed`, usability_fail -> `incomplete`, broken -> `failed`, and a cell with no status record (every `bench` run) -> `incomplete`; ours stays in `myna_status`. `model_repo_id` (20) is the first served model, `model_revision` (21) the snap revision, `engine_kind` (22) the engine read back. `env_os` (35) is the kernel release; `cpu_load_mean_pct` (51) is null (not sampled). The LLM-only columns (23-28 parallelism and arrival, 39-43 TTFT and tokens/s, 54-57 speculative decoding and KV cache) are absent. Ours: the cell's `summarize` numbers (`wer`, `cer`, `wer_whisper`, `cer_whisper`, `rtfx`, `finalize_s_p50`, `finalize_s_p95`, each with `_ci_low`/`_ci_high`, plus `finalize_s_p99`, `timed_clips`, `repeat_cv`, `rtf_median`, `cold_ready_s`, `warm_ready_s`, `ci_resamples`, `ci_seed`), what ran (`label`, `snap`, `mode`, `pace`, `settings`, `artifacts`, `served_runtime`, `environment` as JSON, `environment_digest`, schedule) and the telemetry verdict.
- Samples (`SAMPLES_SCHEMA`, lines 61-74): `unit_id` (63), `ok` (71) and `error` (72, the backend's error code; the message is `error_message`). `request_class` (65) carries our `category` under AIB's name; `consumer_id`, token counts and `ttft_ms` (64, 66-70) have no ASR meaning and are absent. Every other row field ships under its JSONL name, plus `phase`, `repeat`, `edits_sub`/`_del`/`_ins`.
- Load (`LOAD_SCHEMA`, lines 76-86): `unit_id`, `t_offset_s` (the trace's `t`), `ram_bytes` (`rss_mb` x 1e6), `gpu_pct` (mean utilisation over GPUs), `vram_bytes` (`vram_mb` x 2^20, nvidia-smi MiB) and `power_w` (sum over every GPU, null if any went unread; AIB reads GPU 0 alone) keep AIB's names; `cpu_pct` is null. Ours: `cpu_mhz` (per core), `cpu_temp_c`, `cpu_energy_j`, `cpu_throttle_count` and `gpus`, each reading as sampled.

WER and CER are micro-averaged. Speed is audio duration divided by decode time. Final latency measures end-of-audio to committed text; cold load measures session open to ready. Incomplete rows sort behind successful rows because partial metrics are not comparable.
