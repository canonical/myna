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

> **⚠ Open:** the lab (`dev/matrix.py`) writes `results/streaming-tiers.json`,
> and nothing yet promotes that file into an installed location, so `auto`
> currently resolves to batch everywhere unless `$MYNA_TIER_TABLE` is set.
> Staging it into the snap is the open packaging task.

## Testbed client (`myna-dictate`)

One-shot override (highest precedence):

```sh
myna-dictate --socket /path/to.sock --mode streaming --clip clip.wav
myna-dictate --socket /path/to.sock --mode batch --clip clip.wav
```

Persistent preference (used when `--mode` is absent):

```sh
# $XDG_CONFIG_HOME/myna/settings.json (default ~/.config/myna/settings.json)
{ "streaming_mode": "batch" }
```

Edit the file directly; the setting takes effect on the next run and survives
restarts (T046-verified).

## Desktop client (`myna-desktop`)

The desktop app reads the same `streaming_mode` key from the same
`settings.json`, and the resolved mode does double duty: it also decides
**streaming preedit** (in-field unstable hypotheses). Resolving to `streaming`
turns preedit on wherever the injector has a real preedit region; resolving to
`batch` leaves injection commit-only. `myna-desktop --preedit` /
`--no-preedit` override that for debugging. See `docs/desktop-injection.md`
§Streaming preedit.

> **⚠ Open (T54):** the intended snap-confined path is snap config —
> `sudo snap set myna streaming-mode=batch`. That is **not wired**:
> `myna-snap/snap/snapcraft.yaml` declares no `configure` hook, so `snap set
> myna ...` is currently a no-op. Until it lands, edit `settings.json` (under
> the snap's `$SNAP_USER_DATA`) or export `MYNA_TIER_TABLE` for the gate. The
> config-hook work is tracked in `docs/deployment-architecture.md` §5.

The GTK settings UI exposing this key is a follow-up (UD136 design thread).

## How it interacts with the server

The mode is a **display/injection** preference, not a wire negotiation. The
server advertises what it emits via `session.streaming` on the greeting
(additive; absent = batch). A client in batch mode receiving progressive
committed deltas accumulates them and commits at `done` (degenerate-streaming
behavior, FR-010); a client in streaming mode on a batch server simply gets
one segment (edge case 3 in the spec).
