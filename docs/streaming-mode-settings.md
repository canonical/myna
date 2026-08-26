# Streaming Mode Settings

**Feature**: 007-streaming-mode | **Date**: 2026-07-27 (revised 2026-08-24:
one shared resolver, and streaming preedit now rides this setting)

The transcription mode (streaming / batch / auto) is a persisted user setting
(FR-003). This document describes the setting mechanism per client.

## Setting values

| Value | Behavior |
|-------|----------|
| `auto` (default) | Resolve via the hardware-tier RTF gate (see *The baseline* below): streaming only if a measured RTF < 1.0 exists for this hardware; batch otherwise (and on unmeasured tiers). |
| `streaming` | Force streaming display regardless of tier (user accepts latency degradation on weak hardware). |
| `batch` | Force batch display regardless of tier: text appears only at end-of-utterance. |

## One resolver, two binaries

`myna_core::effective_mode(preference)` is the single host-side entry point:
it loads the baseline, fingerprints the machine, and resolves. Both
`myna-dictate` and `myna-desktop` call it, so they cannot drift. The pure gate
(`resolve_mode` / `streaming_viable` / `streaming_viable_here`) stays separate
and is where the semantics are pinned by unit tests.

The model axis is left open (`streaming_viable_here`): which model the server
serves is not knowable before a session opens, so `auto` takes the most
permissive outcome over every model measured on this hardware. Safe, because
the server gates itself too.

### The baseline

`effective_mode` looks for `streaming-tiers.json` in order:

1. `$MYNA_TIER_TABLE` — explicit override, for the lab and for tests
2. `$SNAP/usr/share/myna/streaming-tiers.json` — the packaged copy
3. `/usr/share/myna/streaming-tiers.json` — a system install

Missing or unparseable yields an empty table, so `auto` resolves to **batch**
(FR-010). A baseline is measured data and is never inferred: absent must read
as "unmeasured", not as "assume it streams".

> **⚠ Open (T77): the tier key is not a sensible comparison.**
> `hardware_tier()` returns `<arch>-cpu-generic`, so an RTF measured on one
> machine would be applied to *every* machine of that architecture - a 16-thread
> Ryzen with AVX-512/VNNI and a two-core netbook read as the same tier. A
> baseline was promoted and staged into the snap on 2026-08-26 and then dropped
> the same day for exactly that reason: the RTF is measured, but the claim that
> your machine matches it is inference, and invisible inference at that.
> So nothing installs a baseline today and `auto` means batch everywhere, which
> is the safe end of the failure. The direction under investigation is measuring
> the machine you are actually on rather than classifying it; see T77.

## Testbed client (`myna-dictate`)

One-shot override (highest precedence):

```sh
myna-dictate --socket /path/to.sock --mode streaming --clip clip.wav
myna-dictate --socket /path/to.sock --mode batch --clip clip.wav
```

Persistent preference (used when `--mode` is absent):

```sh
gsettings set org.myna.dictation streaming-mode batch
gsettings get org.myna.dictation streaming-mode
```

A running `myna-desktop` picks the change up live - it subscribes to the store
rather than reading it once at startup, so in-field partials appear (or stop)
from the next hypothesis, with no restart. `activation` and `hotkey` are the
exception: both are bound into the trigger at startup, and a change to either
says so in the journal instead of pretending to apply.

Unpackaged builds need the schema on the host first - `make install-schema` -
since without it every read is the default (`auto`) and there is nothing to
subscribe to.

## Where the setting lives (2026-08-26)

GSettings, schema `org.myna.dictation`, key `streaming-mode`; the source is
`client/data/glib-2.0/schemas/`. It was a JSON file at
`~/.config/myna/settings.json` until 2026-08-26, and that could not work: the
store has to be writable by the confined snap, by unconfined host tools, and
later by other snaps with configuration APIs (T54), while inside the snap
`$HOME` is `$SNAP_USER_DATA` and the `home` interface grants no top-level
dotfiles - so the packaged daemon could never read what the CLI wrote.

There is no automatic migration, deliberately: the only reader the old file
ever had was an unpackaged build, so the one-line `gsettings set` above is the
whole migration.

Two things make it work under confinement, both in `myna-snap/snap/snapcraft.yaml`:
the snap ships and compiles its own copy of the schema
(`GSETTINGS_SCHEMA_DIR`) plus the dconf backend module (`GIO_MODULE_DIR`,
because glib comes from the base and would otherwise scan the base's empty
module dir), and `XDG_CONFIG_HOME` points at `$SNAP_REAL_HOME/.config` so
libdconf opens the *host's* database rather than a snap-private one nothing
writes. Reads and writes were verified in both directions on 2026-08-26.

## Desktop client (`myna-desktop`)

The desktop app reads the same `streaming-mode` key from the same schema,
and the resolved mode does double duty: it also decides
**streaming preedit** (in-field unstable hypotheses). Resolving to `streaming`
turns preedit on wherever the injector has a real preedit region; resolving to
`batch` leaves injection commit-only. `myna-desktop --preedit` /
`--no-preedit` override that for debugging. See `docs/desktop-injection.md`
§Streaming preedit.

The daemon logs what it resolved at every start, so "why are partials not
showing" is answerable from the journal:

```
settings: streaming-mode Auto resolves to Batch on tier x86_64-cpu-generic
```

`snap set myna streaming-mode=…` is deliberately *not* a key: the emission mode
is a per-user preference, and snapd configuration is per snap and root-set. The
system-wide plane covers `activation`, `language` and `hotkey` only.

The GTK settings UI exposing this key is a follow-up (UD136 design thread).

## How it interacts with the server

The mode is a **display/injection** preference, not a wire negotiation. The
server advertises what it emits via `session.streaming` on the greeting
(additive; absent = batch). A client in batch mode receiving progressive
committed deltas accumulates them and commits at `done` (degenerate-streaming
behavior, FR-010); a client in streaming mode on a batch server simply gets
one segment (edge case 3 in the spec).
