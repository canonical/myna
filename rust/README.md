# myna orchestrator (Rust) — Workstream G

The client-side dictation brain: the session + model-residency FSM from
`../docs/architecture/ie115-lifecycle.md`, mediating the audio adapter, the
inference snap, the hotkey and the text injector. See `../docs/project-plan.md`
Workstream G (T38–T43) for the phased plan.

## Crates

| crate | role | task |
|---|---|---|
| `myna-core` | wire contract: events, session config, protocol version, audio types + JSON codec, mirroring Python `myna.core` | **T38 (done)** |
| `myna-orchestrator` | the two-region async FSM + boundary traits (`BackendClient`, `AudioSource`, `Trigger`, `TextSink`) | T39–T40 |
| `myna-cli` (`myna-dictate`) | demo binary wiring mocks end-to-end against the real Python server | T41 |

## Design commitments

- **Wire-agnostic FSM.** The FSM speaks the *existing* `myna.core` wire first
  (real end-to-end runs against the running Python `myna-server` today); IE115
  event names are layered on in T43 as a second `BackendClient`, not a rebuild.
- **Every boundary is a trait with a mock.** The existing Python `myna-server`
  stands in for the inference snap (Ivano); a WAV source for the audio adapter
  (Matias, per `../docs/audio-adapter-api.md`); stdin for the hotkey; stdout for
  the injector. Real implementations drop in behind the same traits.
- **Invariants** (from `../CLAUDE.md`): never persist audio, bounded in-memory
  buffering, no transcription/audio content logged by default.

## Wire parity

`myna-core`'s codec is verified against golden JSON frames captured from Python
`myna.core` (`event_to_wire`, `session_config_to_wire`, the handshake frames).
Frames are compared as parsed JSON values, so key order / whitespace differ but
the structure matches — both ends parse JSON.

## Build & test

```sh
cd rust
cargo test
```
