# Preface

Read this document when changing persisted client preferences, streaming-mode resolution, or live settings reload.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

Client settings use GSettings schema `com.canonical.Myna.Dictation` with the keyfile backend. Packaged and unpackaged clients use the same schema and storage shape.

# Architecture

The snap stores settings below `$SNAP_USER_COMMON/.config`; unpackaged development uses the host configuration directory and requires `make install-schema`. `myna_core::Settings` is the shared access layer.

`myna_core::effective_mode` is the single resolver used by `myna-dictate` and `myna-desktop`:

- `batch` delays display and injection until the utterance completes.
- `streaming` displays committed deltas as they arrive and enables preedit when supported.
- `auto` enables streaming only from measured local capability data; absent or invalid measurements resolve safely to batch.

The mode is a client presentation preference, not wire negotiation. A streaming backend can feed a batch client, which accumulates committed deltas until completion.

`silence-timeout` (unsigned seconds, 0 = off, schema default 30) is the toggle session's idle limit. `Settings::default()` carries the schema default too, so a machine without the schema still ends a forgotten session. The daemon reads it live at every stats tick, so a change applies to the session in progress. Myna Settings renders any bounded integer key as a spin row; a key with a GVariant type the adapter cannot widen to `i64` fails the whole page, so add the conversion before adding such a key.

# Important

- Keep one resolver for all client binaries.
- Missing or invalid performance evidence must resolve to batch, never inferred streaming support.
- Apply live-reloadable settings without restart. Activation and hotkey changes require rebinding and must report that limitation.
- Use command-line overrides for debugging without mutating persisted preferences.
