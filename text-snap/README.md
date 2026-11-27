# text-snap - Myna's text models

`myna-text` serves the generic text models of the dictation processing graph:
today removing spoken disfluencies (English, French, German, Spanish). Translation is the
`myna-translation` snap (`translation-snap/`), a separate share on the same
`myna:backend` plug.
It speaks `myna-text-v1` (JSON lines over a Unix socket) through the same
`inference-provider` content share as the speech snaps, with `MYNA_ROLE=text`
in `provider.env`, so it connects to `myna:backend` beside a speech snap.

Models are snap components, found at run time under `$SNAP_COMPONENTS`; a
component installed while the daemon runs is offered on the next
`capabilities` call.

| component | model | task | measured (warm, 1 thread) | licence |
|---|---|---|---|---|
| `model-disfluency-en` | MiniLM-L12 tagger, int8 ONNX, 46 MB, `myna-disfluency-en` | `text.cleanup` (`language=en`) | p95 5 ms | Apache-2.0 (CC-BY-4.0 and MIT inputs, see its NOTICE) |
| `model-disfluency-fr-de-es` | Multilingual MiniLM-L12 tagger, int8 ONNX, 51 MB, `myna-disfluency` | `text.cleanup` (`language=fr,de,es`) | p95 8 ms | Apache-2.0 (CC-BY-4.0, CC BY 2.0 FR and MIT inputs, see its NOTICE) |

The latencies and quality in each component's `myna-text-model.json` are what
the daemon declares in `capabilities`; they come from the model benchmarks of
the processing-graph workstream, not from a guess.

## Build

```bash
make snap-text
```

That stages `components/` (the taggers from `MYNA_TAGGER_EN_SRC` and
`MYNA_TAGGER_SRC`, checked against pinned sha256 - they have no public home
yet), the wheels and their pinned requirements, and packs.

## Install

```bash
sudo snap install --dangerous ./myna-text_*.snap \
    ./myna-text+model-disfluency-en_*.comp \
    ./myna-text+model-disfluency-fr-de-es_*.comp
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
