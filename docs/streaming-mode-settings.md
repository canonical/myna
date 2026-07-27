# Streaming Mode Settings

**Feature**: 007-streaming-mode | **Date**: 2026-07-27

The transcription mode (streaming / batch / auto) is a persisted user setting
(FR-003). This document describes the setting mechanism per client.

## Setting values

| Value | Behavior |
|-------|----------|
| `auto` (default) | Resolve via the hardware-tier RTF gate (`results/streaming-tiers.json`): streaming only if the active model's measured RTF < 1.0 on this hardware; batch otherwise (and on unmeasured tiers). |
| `streaming` | Force streaming display regardless of tier (user accepts latency degradation on weak hardware). |
| `batch` | Force batch display regardless of tier: text appears only at end-of-utterance. |

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

## Desktop client (`myna-desktop`) — snap-confined

The desktop app reads the same `streaming_mode` key. Under strict confinement
the setting is bound to snap config (T54 wiring):

```sh
sudo snap set myna streaming-mode=batch    # batch | streaming | auto
snap get myna streaming-mode
```

The GTK settings UI exposing this key is a follow-up (UD136 design thread);
until it lands the snap-config CLI above is the supported mechanism (this is
the "(or document CLI for snap config)" path from the task plan).

## How it interacts with the server

The mode is a **display/injection** preference, not a wire negotiation. The
server advertises what it emits via `session.streaming` on the greeting
(additive; absent = batch). A client in batch mode receiving progressive
committed deltas accumulates them and commits at `done` (degenerate-streaming
behavior, FR-010); a client in streaming mode on a batch server simply gets
one segment (edge case 3 in the spec).
