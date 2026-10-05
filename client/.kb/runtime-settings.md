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

`silence-timeout` (unsigned seconds, 0 = off, schema default 30) is the toggle session's idle limit. `Settings::default()` carries the schema default too, so a machine without the schema still ends a forgotten session. The daemon reads it live at every stats tick, so a change applies to the session in progress. Myna Settings renders any bounded integer key as a spin row; a key with a GVariant type the adapter cannot widen to `i64` fails the whole page, so add the conversion before adding such a key.

`sounds` (boolean, schema default true) turns the session cues on and off (`.kb/desktop-integration.md`). The daemon reads it live, at the next cue. Myna Settings renders any boolean key as a switch row.

`sound-set` (enum `myna` | `tine` | `hum`, schema default `myna`) picks which cues play; `myna_core::SoundSet` mirrors the enum, and a value outside it reads as the default, never as silence. The daemon reads it live, at the next cue. Myna Settings shows it as "Sound style" (Bird, Chime, Voice) with a Preview button that asks the daemon to play the selected set over D-Bus and is insensitive while `sounds` is off.

# Important

- Apply live-reloadable settings without restart. Activation and hotkey changes require rebinding and must report that limitation.
- Use command-line overrides for debugging without mutating persisted preferences.
