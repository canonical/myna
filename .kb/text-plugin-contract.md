# Preface

Read this when changing a text model manifest (`myna-text-model.json`), the `capabilities` reply of a `myna-text-v1` provider, or how the client turns a reply into a graph step. It governs both text-provider snaps, `myna-text` and `myna-translation`.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

A text plugin is a model component whose manifest declares a graph step as data. The provider (`server/src/myna/text/protocol.py`) echoes the manifest in `capabilities`; the client (`client/myna-desktop/src/process/wire.rs`, `step.rs`) reads it. A new model of a known task, or a new language pair, needs no Rust change. Only a new class does, and a class this client does not know makes the model uncovered: the resolver logs the share and model id (never text) and does not offer it.

# Architecture

## Contract 2 fields

Manifest keys, all optional, and the same names in each model of `capabilities`; the reply also carries a top-level `contract: 2` (absent means 1).

- `class` - `edit`, `translate` or `rewrite`. The client derives the output `Check` from it (`edit` to `ConservativeEdit`, `translate` to `Usable{translation:true}`, `rewrite` to `Usable{translation:false}`). A plugin cannot name or disable a check. Absent: the task's class (`text.cleanup` edit, `text.translate` translate, `text.rewrite` rewrite); the provider emits that default itself, and the client applies it again for a provider that does not.
- `needs` - `sentence` or `dictation`: how much text one call wants.
- `params` - descriptors `{name, kind: language|text, role: source|target|free, required}` so Settings can render a picker without knowing the task.
- `license`, `attribution`, `source` - strings; `download_bytes` - the component's size.

`in_lang` and `out_lang` of a step are not fields: `covers.language` is the input language and `covers.to` the output language, or `covers.language` again when the model has no `to`. They stay the one copy of that data.

The manifest also names `contract` (default 1). A provider skips, and logs by component name, a manifest naming a contract above its own or a field of the wrong shape.

## Rules

- Additive within a contract number: a field may be added; both sides ignore keys they do not know and treat a missing one as the task default. An old provider with a new client, and the reverse, keep working.
- Changing the meaning of a field, or the class list's meaning, bumps `contract`. Settings hides entries whose contract it does not understand.
- The protocol string stays `myna-text-v1`. A breaking wire change would be `myna-text-v2`, which a provider may answer alongside v1 for one release.
- `covers` and `accepts` keep their meaning: coverage is by request `(task, params)`, never by model id.
- Never dictated text in a reply to a refusal or in a log line.

`server/tests/text/test_protocol.py` reads `wire.rs` and fails when the Python capability and the Rust structs list different fields.

## One source per plugin

Nobody edits a manifest, NOTICE, `components:` stanza or catalog by hand. A plugin is `<snap-dir>/plugins/<name>.toml` (`<name>` is the component minus `model-`) plus `<name>.measured.json`. `dev/gen-plugins.py <snap-dir>` writes, deterministically:

- the `components:` stanza and the `model-components` `organize:` mappings of `snap/snapcraft.yaml`, between the `BEGIN/END generated` marker comments;
- `models/<name>/myna-text-model.json` and `models/<name>/NOTICE` (the notice text plus the sha256 of every file);
- `<snap-dir>/catalog.json`, shipped at `$SNAP/share/<snap>/catalog.json` (staged by `dev/prepare.sh`) and snapshotted for both snaps into the myna-config deb (`/usr/share/myna-config/catalog/<snap>.json`, by `myna-config-deb/build-source.sh`).

`make lint-plugins` (part of `make check`) and `server/tests/test_plugin_catalog.py` regenerate in memory and fail on drift. Component names `model-<kind>-<lang...>` are unique across both snaps. `<snap-dir>/dev/download-models.sh` only calls `dev/stage-plugins.sh`, which fetches (a `url`, checked against the pinned sha256) or copies (a `[local]` directory) what the toml lists.

- Allowed licences: Apache-2.0, MIT, MPL-2.0, CC-BY-4.0, BSD. Anything else fails validation; a licence other than Apache-2.0 and MPL-2.0 names its `license_file`.
- `quality`, `quality_source` and `latency_ms` live only in `<name>.measured.json`, written by the evaluation pipeline (`dev/plugin-pipeline.sh`); never in the toml.
- `entry_revision` is bumped whenever an entry changes; Settings merges catalogs by entry `id` (the component name), the installed snap's catalog over the deb snapshot, the higher `entry_revision` winning.
- Labels are derived keys, never free text: `step.<step id>.name`, `.description` and `.example`, translated through gettext. A translate entry's pair label is built from `covers` and the language endonyms.
- Catalog entry: `id`, `snap`, `component`, `model`, `contract`, `entry_revision`, `task`, `class`, `needs`, `params`, `covers`, `in_lang`, `out_lang`, `step{id, stage, label_key, description_key, example_key}`, `download_bytes` (the component squashfs), `installed_bytes` (sum of the file sizes), `quality{value, source}`, `latency_ms`, `latency_class` (`fast` under 50 ms, `ok` under 400, else `slow`), `hardware{arch, min_ram_mb}`, `locales` (languages it is useful for), `curated`, `license`, `attribution`, `source`, `files[{name, sha256, bytes, url?}]`. Stages: `field-fit`, `clean-up`, `translate`, `target-language`.
- A model that needs a config file keeps it hand-written in `models/<name>/` and names it with `config`.

## Evaluating and gating a plugin

`myna-bench plugin-scaffold --pair en:<l> [--arch tiny] [--no-eval]` writes the first draft of a translation plugin (`plugins/translate-en-<l>.toml`, a pending `.measured.json` seed and `models/translate-en-<l>/config.yml`) from the Remote Settings `translations-models` records and the model's `metadata.json` (vendor FLORES COMET; the architecture is the one whose `hash` matches the record, base-memory first, tiny as the fallback). `download_bytes` is the file sum until the component is packed, `spot_check` is left to a native speaker, and a language without a FLEURS locale is refused without `--no-eval`. Review the toml, then run the pipeline below. Tests use a recorded fixture (`server/tests/benchmarker/data/remote_settings_fixture.json`), never the network.

`~/myna-processing-graphs/chug.sh run dev/plugin-pipeline.sh <plugin-id>` is the one command per plugin, on chug only (no weights on the laptop). It refuses a licence off the allow-list or a stale NOTICE, fetches the pinned files and refuses any whose sha256 or size differs from the toml, stages the component in a scratch directory, serves it through the real provider daemon (`myna.translation` or `myna.text`) on a socket, grades it with `myna-bench text --plugin` and writes `results/plugins/<id>.json` (chrF, p50/p95, peak RSS of the daemon tree, hypotheses, versions, hashes, degeneracy rates; under `$MYNA_RESULTS_DIR`, default `~/myna-processing-graphs/results`) plus the slim `<name>.measured.json`, then reruns `dev/gen-plugins.py`. Exit 0 gate passed, 1 gate failed (files still written), 2 refused. It needs `myna-bergamot` for the translation snap (`$MYNA_BERGAMOT`, `PATH`, or the installed snap).

The gate is DECISIONS 7.4 and lives in the plugin's optional `[gate]` table, every key defaulted (`server/src/myna/benchmarker/text_gate.py`): `vendor_comet` (Mozilla's FLORES COMET from the model's `metadata.json`; must be >= `comet_min` 0.80; omit it for "quality not measured"), `vendor_chrf` x `chrf_ratio` 0.85 where the vendor published a chrF (they publish BLEU and COMET only) else `chrf_floor` 0.40, `chrf_min` to override (en-fr keeps 0.58), `p95_max_ms` 400, `empty_max` 0, `repeat_max` 0.01, `ratio_min`/`ratio_max` 0.5/2.0 (median output length over the reference's, in characters), `copy_max` 0.05. A plugin whose pair is `X->en` is graded on the same slice reversed. A pair with no corpus slice (no FLEURS locale, or too few FLEURS sentences: af, eu, gl, is, mr, sq, ug, ur, zh-Hant) is probed instead (`probe_<l>`): the 200 `clean_en` sentences go in whatever its source language, and it passes when every output is non-empty, loop-free and in time and, where Mozilla publishes one, the vendor COMET clears 0.80. Its quality is the vendor COMET, else 0, and its source says it was not measured. Mozilla serves newer models (2.x) without a `metadata.json` in the repo; the scaffold takes the newest record anyway and names its architecture `mozilla`, with no vendor COMET (0.40 chrF floor).

A native-speaker spot check is optional and recorded by hand in the toml as `[spot_check]` with `who`, `date`, `passed` (optional `notes`). The generator emits `curated: true` when the file asks for it and, if it has a `[gate]`, `measured.json` says `gate: pass`; a spot check recorded as failed unlists it. The numeric gate alone lists a pair (DECISIONS 7.4, 2026-11-23: ship on the numbers, look closer where users report a pair is poor). A pair that fails the gate ships unlisted, and only `MYNA_LIST_UNCURATED=1` in Settings' environment shows it (development and E2E).

Corpus slices for a language: `myna-bench build-text-corpus --pair en:<l>` (a Mozilla code; `fleurs_codes.py` maps it to a FLEURS locale, `nn`, `sq` and `zh-Hant` are no-eval) from the cached FLEURS `test.tsv` files (`~/.cache/myna/corpus-src/fleurs/<locale>/`); commit the new `translate_en_<l>.jsonl`, `SHA256SUMS` and `NOTICE`.

