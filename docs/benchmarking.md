# Benchmarking cheat sheet

One tool does all of it: `myna.benchmarker`, run in-tree as
`python -m myna.benchmarker` (the `make bench-*` targets) and shipped to other
machines as `myna-bench.pyz`. Same code both sides, so a number measured on a
lab box and a number measured here mean the same thing.

The shape of the exercise:

```
   here                        the other machine              here
   ────                        ─────────────────              ────
   make snaps + make bench ─►  plan / check / run     ──►     make bench-merge
   (copy .snap/.comp/.pyz)     (results.jsonl)                (leaderboard)
```

## 0. Try it here first

Do this before shipping anything. It is the same code path, and a mistake found
locally costs minutes rather than someone else's afternoon.

```shell
make bench-check                # is this machine fit to measure on?
make bench-plan                 # every row, and how long it would take. No root.
make bench-corpus               # build corpus/english/manifest-balanced.json (~330 MB, once)
make bench-run-whisper          # one snap end to end (sudo; installs and purges)
make bench-aggregate            # the table
```

`bench-plan` needs neither root nor snapd and is the fastest way to see whether
a config change did what you meant. `bench-run-whisper` is the smallest real
sweep - it exercises install, engine selection, model switching, both emission
modes and the config axis.

**`bench-run` installs and removes snaps, purging their data.** Do not run it on
a machine where you use Myna for real.

## 1. What to copy over

```shell
make snaps                      # or just the ones you care about
make bench                      # -> myna-bench.pyz
```

Copy to the other machine:

- `myna-bench.pyz`
- the packed artefacts for the snaps you want: `*-snap/myna-*_*.snap` and their
  `*-snap/myna-*+*.comp` components (a snap without its model component installs
  but cannot serve)
- a `bench.yaml` - start from `dev/bench.yaml.example`, which already spells out
  the one thing that is easy to get wrong: `cli:` is the modelctl command, and
  it is named after the *adapter*, not the snap (`myna-funasr.funasr`, not
  `myna-funasr`). A wrong `cli:` loses that target with a confusing error.

The corpus is not copied. It is rebuilt on the far machine, which is exact:
FLAC decode is lossless and the noise seed is fixed, so the same arguments
produce the same audio and the same `corpus_id`. That is several hundred MB you
do not move, and `summarize` refuses to mix corpora, so a mistake here is caught
rather than averaged in.

## 2. On the other machine

Needs Ubuntu with snapd, `python3`, and `ffmpeg`.

```shell
sudo apt install ffmpeg

# a. the corpus. Its id must match the one bench.yaml names.
python3 myna-bench.pyz download-corpus --out ./corpus \
    --select balanced -n 80 --manifest-name manifest-balanced.json \
    --long-form-minutes 5

# b. is the machine fit to measure on? It reports; you decide.
python3 myna-bench.pyz check --sweep

# c. what will be measured, and for how long. No root.
python3 myna-bench.pyz plan --config bench.yaml

# d. the sweep. Installs and REMOVES snaps.
sudo python3 myna-bench.pyz run --config bench.yaml

# e. their own numbers, before sending anything
python3 myna-bench.pyz summarize --in results.jsonl --by-category
```

Then send back `results.jsonl`. It carries a machine summary (CPU, RAM, GPU,
kernel), the corpus id, one row per clip × snap × model × mode × config, and a
status row per cell. No audio, no personal data.

The usual finding from `check --sweep` is a non-performance CPU governor:

```shell
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do
    echo performance | sudo tee $g
done
```

A `run` can be resumed a snap at a time if it is interrupted:

```shell
sudo python3 myna-bench.pyz run --config bench.yaml \
    --only myna-parakeet --keep-results
```

## 3. Merge into the leaderboard

`results/leaderboard.jsonl` is one tracked file holding every submission.

```shell
make bench-merge SUBMISSIONS="incoming/framework.jsonl incoming/thinkpad.jsonl"
git add results/leaderboard.jsonl && git commit -m "bench: add <machine> results"
```

A row's identity is **(machine, label)**, so the same
`<snap>/<engine>/<model>/<mode>` from two machines is two rows, ranked against
each other. Re-running a machine replaces that machine's rows rather than
doubling them, so the file can be rebuilt from whatever submissions are to hand.

Reading it:

```shell
cd server
uv run python -m myna.benchmarker summarize --in ../results/leaderboard.jsonl \
    --by-category --sort speed
```

`--sort` takes `wer` (default), `cer`, `speed`, `latency`, `cold-load` or
`label`. Rows that did not finish always sort last whatever you pick: their
numbers came from however many clips they got through, so they are not
comparable and must never appear to have won.

Two things `merge` refuses, because both lose data silently:

- **a submission scored against a different corpus.** A WER micro-averaged
  across two corpora ranks nothing. Rebuild the corpus with the arguments in
  `bench.yaml`'s header comment, or pass `--allow-mixed-corpora` and then always
  `summarize --corpus <id>`.
- **two hosts sharing a hostname.** Rows are keyed by name; two laptops both
  called `framework` would merge into one row. Rename one.

## What the numbers mean

Labels are `<snap>/<engine>/<model>/<mode>[-<config>]`. The engine is not
configured - `use-engine --auto` picks it by hardware detection and the runner
reads back what it landed on, so a label is a record of what actually ran.

| Column | |
| --- | --- |
| `WER% / CER%` | micro-averaged over warm clips: total edits / total reference, so long clips count proportionally |
| `speed` | audio ÷ decode time, higher is faster (`22x` = 22 seconds of speech per second of compute) |
| `med/p95 final` | end of audio → committed text |
| `cold load` | session open → ready, from the `--cold` sample: the model-load wait a user feels on their first dictation after idle |
| `RSS/VRAM` | peak during the run |
| `status` | `OK`, `USABILITY_FAIL` (ran out of budget mid-sweep, metrics are partial), `BROKEN` (crashed) |

A `USABILITY_FAIL` is a result, not an error to retry: the budget is "must beat
0.83× real time end to end", and a backend that cannot is a product finding.

## Adding an axis

Axes live in `dev/matrix.yaml`. Models and emission mode come from the snap
itself; anything else is a `configs:` entry naming the modes it applies to:

```yaml
- snap: myna-whisper
  dir: whisper-snap
  configs:
    - label: int8
      modes: [batch, streaming]     # decode precision affects both
      settings: {compute-type: int8}
    - label: arm3s
      modes: [streaming]            # a latency dial means nothing in batch
      settings: {stream-arm-seconds: "3"}
```

A mode no entry claims still gets exactly one row at whatever the snap shipped,
and keys an entry omits are restored to the shipped value, so every row is an
absolute configuration rather than a difference from the row before it. Only
keys some `engine.yaml` declares are accepted - `make bench-plan` catches a
typo before the sweep does, hours in.

## Troubleshooting

| Symptom | |
| --- | --- |
| `plan` says a target is not packed | `make snap-<name>`, then copy the new artefacts over |
| a target is `BROKEN` immediately | `journalctl -u snap.<snap>.server` on that machine; usually a component that did not install |
| `no engine could be selected` | the snap ships only a GPU engine and there is no GPU (nemotron is commented out of `matrix.yaml` for exactly this) |
| WER is ~100% on long-form only | a long clip fed flat out can outrun a backend's websocket keepalive; re-check that row with `myna-bench bench --realtime` |
| corpus id does not match | the `download-corpus` arguments differ from the ones used for the leaderboard's corpus |
| `not a complete LibriSpeech archive` | an earlier download was interrupted; the message names the file to delete |
