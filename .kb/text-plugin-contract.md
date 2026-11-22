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
