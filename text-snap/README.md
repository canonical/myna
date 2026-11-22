# text-snap - Myna's text models

`myna-text` serves the small text models of the dictation processing graph:
removing spoken disfluencies (English) and translating (English to French).
It speaks `myna-text-v1` (JSON lines over a Unix socket) through the same
`inference-provider` content share as the speech snaps, with `MYNA_ROLE=text`
in `provider.env`, so it connects to `myna:backend` beside a speech snap.

Models are snap components, found at run time under `$SNAP_COMPONENTS`; a
component installed while the daemon runs is offered on the next
`capabilities` call.

| component | model | task | measured (warm, 1 thread) | licence |
|---|---|---|---|---|
| `model-disfluency-en` | MiniLM-L12 tagger, int8 ONNX, 46 MB | `text.cleanup` (`language=en`) | p95 5 ms | Apache-2.0 (CC-BY-4.0 and MIT inputs, see its NOTICE) |
| `model-translate-en-fr` | Firefox Translations base-memory, 36 MB | `text.translate` (`language=en`, `to=fr`) | p95 19 ms, chrF 0.70 | MPL-2.0 |

The latencies and quality in each component's `myna-text-model.json` are what
the daemon declares in `capabilities`; they come from the model benchmarks of
the processing-graph workstream, not from a guess.

## Build

```bash
make snap-text
```

That stages `components/` (the translation model from Mozilla's CDN, checked
against pinned sha256; the tagger from `MYNA_TAGGER_SRC`, checked against
pinned sha256 - it has no public home yet), the wheels and their pinned
requirements, and packs. `myna-bergamot` is built from a pinned
bergamot-translator commit with `USE_MKL=OFF`: the PyPI `bergamot` wheel embeds
Intel MKL (proprietary) and is not used.

## Install

```bash
sudo snap install --dangerous ./myna-text_*.snap \
    ./myna-text+model-disfluency-en_*.comp \
    ./myna-text+model-translate-en-fr_*.comp
sudo snap connect myna:backend myna-text:provider
sudo snap connect myna-text:hardware-observe
```

`hardware-observe` is what onnxruntime's device discovery reads PCI ids through;
unconnected it is a denial in the journal, not a failure.

## Wire

`capabilities` lists the models with `covers`, `accepts`, `latency_ms` and
`quality`; `warm` loads one; `run` applies it. The daemon's side of the wire
is `client/myna-desktop/src/process/wire.rs`; `server/tests/text/` pins the two
together. Replies carry text only on success; an error never quotes the text,
and the daemon logs ops, model ids, durations and outcomes, never the text.

## Translation runtime

`myna-bergamot` (`bergamot/myna-bergamot.cpp`) is bergamot-translator behind a
line protocol: it prints `ready` once the model is built, then answers each
input line with one output line, flushed. The stock `bergamot` CLI reads all
of stdin first, so it cannot serve a dictation one sentence at a time. The
daemon keeps one child per model, restarts it after a timeout or a crash, and
refuses (rather than guesses) when it cannot answer within the deadline.
