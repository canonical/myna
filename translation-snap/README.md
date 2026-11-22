# translation-snap - Myna's translation models

`myna-translation` serves Mozilla's Firefox Translations models, one snap
component per directed language pair, run by bergamot-translator. It speaks
`myna-text-v1` (JSON lines over a Unix socket) through the same
`inference-provider` content share as the speech snaps, with `MYNA_ROLE=text`
in `provider.env`, so it connects to `myna:backend` beside a speech snap and
`myna-text`: each is its own share. A chain such as `tidy-french` binds
`text.cleanup` to a `myna-text` model and `text.translate` to a
`myna-translation` model.

Models are snap components, found at run time under `$SNAP_COMPONENTS`; a
component installed while the daemon runs is offered on the next
`capabilities` call.

| component | model | task | measured (warm, 1 thread) | licence |
|---|---|---|---|---|
| `model-translate-en-fr` | Firefox Translations base-memory, 36 MB | `text.translate` (`language=en`, `to=fr`) | p95 19 ms, chrF 0.70 | MPL-2.0 |

The latency and quality in each component's `myna-text-model.json` are what
the daemon declares in `capabilities`; they come from the model benchmarks of
the processing-graph workstream, not from a guess.

## Build

```bash
make snap-translation
```

That stages `components/` (the model from Mozilla's CDN, checked against
pinned sha256), the wheels and their pinned requirements, and packs.
`myna-bergamot` is built from a pinned bergamot-translator commit with
`USE_MKL=OFF`: the PyPI `bergamot` wheel embeds Intel MKL (proprietary) and is
not used. The snap carries no ONNX Runtime.

## Install

```bash
sudo snap install --dangerous ./myna-translation_*.snap \
    ./myna-translation+model-translate-en-fr_*.comp
sudo snap connect myna:backend myna-translation:provider
```

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
