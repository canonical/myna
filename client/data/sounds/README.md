# Dictation chimes

Source assets for the start/stop/error chimes for good UX. Synthesized via `ffmpeg`'s `sine` source (rising/falling two-note blips for start/stop, two short low beeps for error) — original, no license restrictions.

| File | Role | Duration |
| --- | --- | --- |
| `start.oga` | Recording started | 0.20s |
| `stop.oga` | Finalizing / cancelled | 0.20s |
| `error.oga` | Unrecoverable error | 0.21s |

These are the canonical sources (kept for provenance/regeneration); the shipped binary embeds a decoded PCM copy — see `client/myna-desktop/src/chime.rs` for the regeneration command.
