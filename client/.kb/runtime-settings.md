# Preface

Read this document when changing persisted client preferences, the streaming mode, or live settings reload.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Client settings use GSettings schema `com.canonical.Myna.Dictation` with the keyfile backend. Packaged and unpackaged clients use the same schema and storage shape.

# Architecture

The snap stores settings below `$SNAP_USER_COMMON/.config`; unpackaged development uses the host configuration directory and requires `make install-schema`. `myna_core::Settings` is the shared access layer.

`streaming-mode` has two values and no `auto`:

- `streaming` displays committed deltas as they arrive and enables preedit when supported.
- `batch` delays display and injection until the utterance completes.

The mode in force comes from `myna_core::effective_mode`, the one resolver: the daemon, `myna-testbed` and Myna Settings' "When to transcribe" row all use it. A user value in the store always wins, including one equal to the schema default. Only with no user value does the backend's `Capabilities.streaming` decide: streaming when it streams, batch when it does not. When the backend's answer is unknown (an older server, a failed query, no press yet) the schema default `streaming` applies, and a test pins the resolver's fallback to the schema default. `Settings::streaming_mode` is therefore `Option`: it reads GSettings' user value, and a value outside the schema enum, such as the retired `auto`, reads as no choice. `myna.config reset streaming-mode` hands the choice back to the backend.

Myna Settings has no backend socket, so it feeds the resolver `myna_core::streams_by_default(snap)` for the connected backend: the `streaming` value the snap's install hook sets (Parakeet true, every other family and any unknown snap false), `None` while no single backend is connected. That matches `Capabilities.streaming` until someone sets the engine's `streaming` by hand through modelctl; the divergence is accepted, and the row must not read modelctl to close it, since the Model tab's snapshot is not loaded until that tab opens. The row re-resolves on every backend switch, never writes while displaying, and stores a user value only when the user picks an option; picking the option already shown changes nothing and stores nothing, which is GTK's drop-down behaviour.

The daemon asks the backend with `capabilities.query` at every press, beside the session rather than ahead of it: preedit is read per transcript event, and a backend swap or an engine setting can change the answer behind the same socket. Its journal line names the source of the preedit (flag, the user's choice, the backend's default, or the schema default), and `myna.status` prints it as `flag`, `settings`, `backend` or `built-in`. The query is bounded by `myna_orchestrator::CAPABILITIES_TIMEOUT`, so a wedged backend reads as unknown rather than hanging a press or `myna.status`.

The mode is a client presentation preference, not wire negotiation. A streaming backend can feed a batch client, which accumulates committed deltas until completion.

A binary can be newer than the schema installed beside it (a dev build against an older system schema), and GLib aborts on a read of a key the schema lacks, so every `Store` read answers `None` for one and `Settings` takes that key's `Settings::default()` value.

`silence-timeout` (unsigned seconds, 0 = off, schema default 30) is the toggle session's idle limit. `Settings::default()` carries the schema default too, so a machine without the schema still ends a forgotten session. The daemon reads it live at every stats tick, so a change applies to the session in progress. Myna Settings renders any bounded integer key as a spin row.

`sounds` (boolean, schema default true) turns the session cues on and off (`.kb/desktop-integration.md`). The daemon reads it live, at the next cue. Myna Settings renders any boolean key as a switch row.

`sound-set` (enum `myna` | `tine` | `hum` | `marimba` | `drop` | `radio` | `koto`, schema default `tine`) picks which cues play; `myna_core::SoundSet` mirrors the enum, and a value outside it reads as the default, never as silence. The daemon reads it live, at the next cue. Myna Settings shows it as "Sound style" (Chime, Bird, Voice, Marimba, Water, Radio, Strings) with a Preview button that asks the daemon to play the selected set over D-Bus and is insensitive while `sounds` is off.

`vocabulary` (`as`, default `[]`) is the user's word list (GitHub issue #1): names and terms the model biases recognition toward. Raw and unvalidated in `Settings`/`Store::strings`; a caller builds `SessionConfig.keywords` by running it through `myna_core::normalize_keywords` (in `session.rs`, mirroring the server's `KEYWORDS_MAX_ENTRIES`/`KEYWORDS_MAX_ENTRY_CHARS`), which trims, drops empties, dedupes and caps rather than rejecting - a setting that has drifted past the server's limits must not fail every dictation. `myna-desktop` reads it live per press like `language`, with no argv override; `myna-testbed` takes it via repeatable `--vocabulary`. The IE115 client sends it only to a backend whose greeting lists `keywords` (the top-level `.kb/session-contract.md`); every shipped backend does, each correcting toward it in its own way (`server/.kb/model-evaluation.md`), so a backend that does not costs nothing. Myna Settings edits it from the last row of General's Dictation group, whatever the model, which opens the list on a page of its own so 100 entries push nothing down (`list_editor_ui.rs`), refusing with a message an entry past those same limits, a duplicate or a blank, so the clamp never drops a word the user saw accepted.

Processing is three keys, `myna_core::chains`. `chains` (`a(ssa(sba{ss}))`) is the saved chains in the user's order: each a stable id (rename never touches it, so the active pointer and the pill survive), a display name and its ordered steps, each a step id, an on flag and its own parameters by bare name (`to`, `mode`). Parameters are per step, not the older `<step>.<param>` map, because a chain may hold one step twice (two translations). Ids are stable (`fit`, `tidy`, `translate`, `script`; `myna_core::processing::STEP_ORDER`); an id this build does not know is kept, so an uninstalled plugin's step survives. The default holds one example chain (`EXAMPLE_ID`, a placeholder until `chains-showcase-example`). `active-chain` (`s`, default empty) names the chain in use; empty or an id no chain has is none, which types exactly what was heard. `fit-to-field` (`b`, default true) is the switch that fits text to the field after any chain; the daemon reads it live into the pipeline's output stage (`process::fit`, `desktop-integration.md`), so it is never a step; the daemon compiles only the active chain's on steps, in stage order (`Processing::stage_steps`). `Processing::write` sets what changed in one delayed apply on a throwaway `gio::Settings` over the same backend (delay mode never ends on an object), and every watcher listens to `change-event` (`chains::on_change_event`; gio-rs does not bind it), so one edit is one wake, in this process or another, and never a half-written read: deleting the active chain changes two keys. Myna Settings does not list the three keys as rows. Until the chains page lands, the step-switch page reads `Processing::page_steps` (the stage list with `fit` first when the switch is on) and writes back through `set_stage_steps`, which maps `fit` to the switch and the rest onto the active chain, made as "My steps" when none is; a step switched off stays in the chain as an off step holding its parameters, so a choice (a script mode) outlives its switch. `myna_core::ProcessingChain` and `ChineseScript` name the preset and script nicks; neither is a stored key. `myna_core::processing::migrate_legacy` moves a dev keyfile that still carries `processing`, `processing-tidy`, `processing-translate`, `processing-sentences`, `processing-chain` or `chinese-script` to `processing-steps`, and `migrate_steps` then moves `processing-steps` and `processing-params` to a "My steps" chain made active (id `my-steps`), `fit` among them becoming `fit-to-field`, values already in the new keys winning; both run once, at daemon start and Settings launch, and remove what they read. None of those keys is in a release, so both functions and their callers are deleted in the first release after they ship.

Myna Settings holds `i`, `u`, `b`, `s` (and enums) and `as` keys (`client/myna-config/src/adapters/client_settings.rs`). A key of any other type is left off the page rather than failing it, so a new type needs its conversion and widget before the schema key lands.

# Important

- Apply live-reloadable settings without restart. Activation and hotkey changes require rebinding and must report that limitation.
- Use command-line overrides for debugging without mutating persisted preferences.
