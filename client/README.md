# myna client (Rust) — Workstreams D + G

The dictation **client**: it owns microphone capture and the hotkey, runs the
wire-agnostic session + model-residency FSM (from
`../docs/architecture/ie115-lifecycle.md`) against an inference backend, and
injects transcribed text into the focused application. See
`../docs/project-plan.md` (Workstream D — audio; Workstream G — orchestrator;
T21/T22 — the desktop last-mile) and `../docs/desktop-injection.md`.

## Crates

| crate | role | task |
|---|---|---|
| `myna-core` | wire contract: events, session config, protocol version, audio types + JSON codec, mirroring Python `myna.core` | **T38 (done)** |
| `myna-audio` | native PipeWire capture adapter behind `AudioSource`/`CaptureBackend` (`../docs/audio-adapter-api.md`) — node selection, channel pick/downmix, live device enumeration | **T49–T52 (done)** |
| `myna-orchestrator` | the two-region async FSM + boundary traits (`BackendClient`, `AudioSource`, `Trigger`, `TextSink`) | **T39–T43 (done)** |
| `myna-cli` (`myna-dictate`) | demo binary wiring the boundaries end-to-end against the real Python server (WAV / corpus / live mic) | **T41 (done)** |
| `myna-desktop` (`myna-desktop`) | shipped dictation app: trigger → capture → IBus injection; notifications for bare runs, or `org.myna.Dictation` for the Shell HUD | **T21/T22 (done)** |

## Design commitments

- **Wire-agnostic FSM.** The FSM speaks the *existing* `myna.core` wire first
  (real end-to-end runs against the running Python `myna-server` today); the
  OpenAI-Realtime-shaped **IE115** wire is layered on (T43) as a *second*
  `BackendClient` (`ws_unix_ie115::WsUnixIe115Backend`) — the FSM and driver are
  unchanged, proving the trait boundary. Pick it at runtime with
  `myna-dictate --dialect ie115` (see `../docs/architecture/ie115-wire.md`).
- **Every boundary is a trait with a mock.** The Python `myna-server` stands in
  for the inference snap; `myna-audio` is the capture adapter (mock:
  `ScriptedBackend`); the hotkey is `Trigger` (`StdinTrigger` /
  `GlobalShortcutTrigger`); the injector/indicator are `Injector` / `Indicator`
  (`MockInjector` / `IbusInjector`, `MockIndicator` / `GtkIndicator`). Real
  implementations drop in behind the same traits. `DbusIndicator` publishes the
  state/level contract consumed by `../extensions/myna-shell/`.
- **Invariants** (from `../CLAUDE.md`): never persist audio, bounded in-memory
  buffering, no transcription/audio content logged by default; the desktop
  injector handles **text only** and is commit-only by default; opt-in
  `--preedit` displays unstable streaming text through IBus preedit.

## Wire parity

`myna-core`'s codec is verified against golden JSON frames captured from Python
`myna.core` (`event_to_wire`, `session_config_to_wire`, the handshake frames).
Frames are compared as parsed JSON values, so key order / whitespace differ but
the structure matches — both ends parse JSON.

## Build & test

```sh
cd client
cargo test --workspace                              # everything
cargo test -p myna-desktop --no-default-features    # desktop, hermetic (no GTK/DBus/portal/display)
cargo clippy --workspace --all-targets -- -D warnings
```

Env-gated integration suites run identically on the desktop VM and on hardware,
and skip cleanly otherwise: `MYNA_PIPEWIRE_TESTS=1` (capture), `MYNA_IBUS_TESTS=1`
(injection), `MYNA_PORTAL_TESTS=1` (hotkey), display-present (GTK indicator).

## Run

### Testbed demo — `myna-dictate` (WAV / corpus / live mic)

Against a running Python `myna-server` (any adapter) on a Unix socket:

```sh
# internal myna.core wire (default) — Enter to start, Enter/clip-end to stop, Ctrl-D quits
myna-dictate --socket /tmp/myna.sock --language en --clip corpus/real/audio/<id>.wav

# IE115 (OpenAI-Realtime-shaped) wire — same FSM, second backend
myna-dictate --socket /tmp/myna.sock --dialect ie115 --language en --clip <wav>

# live microphone via the native PipeWire backend (no subprocess)
myna-dictate --socket /tmp/myna.sock --language en --mic
myna-dictate --list-devices
```

### The dictation app — `myna-desktop` (hotkey → IBus injection)

The shipped push-to-talk app. Needs a Wayland/GNOME session and a running IBus
daemon. Activation must not depend on terminal focus (dictation injects into
*another* app), so the default is **toggle-to-talk via a GNOME custom keyboard
shortcut**: the app runs as a daemon on a control socket, and a GNOME shortcut
bound to `myna-desktop --toggle` pokes it (tap = start, tap = stop). This works
for a plain unsandboxed binary — no terminal focus, no portal, no app id.

```sh
(cd ../server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &

myna-desktop --install-shortcut '<Super>t>'      # once: binds a shortcut → `myna-desktop --toggle`
myna-desktop --socket /tmp/myna.sock --language en   # the daemon (leave running)
# focus a text field, tap your shortcut, speak, tap → transcript injected there
```

Other activation modes: `--portal` (GlobalShortcuts hold-to-talk — only works
when packaged as a snap/flatpak, which GNOME grants an app identity); `--stdin`
(terminal debug — injects back into the terminal); `--overlay` (GTK activity
overlay instead of notifications — **experimental**: on GNOME/Wayland the overlay
window can steal focus and cut the session short); `--dbus` publishes
`org.myna.Dictation` for the separately installed focus-safe Shell extension.
Feedback defaults to desktop notifications only for a bare run without `--dbus`.

See `../docs/desktop-injection.md` for the settled T21/T22 contract and
`../extensions/myna-shell/README.md` for the current Basic/Wave Shell HUD.
