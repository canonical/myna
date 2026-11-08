# Myna Fcitx5 addon

This optional addon lets `myna-desktop` commit dictation through Fcitx5.
Without it, Myna continues to use its existing IBus injector.

Fcitx5's IBus compatibility object only creates input contexts. It does not
provide `RegisterComponent` or the engine takeover that Myna's IBus injector
needs, so IBus injection cannot work when Fcitx5 owns the session's input
handling. Running inside Fcitx5 avoids that emulation gap: the addon has the
focused `fcitx::InputContext` and can call `commitString()` directly.

## Architecture

- `myna_fcitx5.so` is loaded by Fcitx5 and listens on a private Unix socket.
- `myna-desktop` discovers the addon socket before connecting IBus.
- The addon binds the most recently focused Fcitx input context.
- Myna sends stable transcript text and volatile preedit to the addon.
- Focus loss closes the target, and Myna falls back to its existing focus-loss
  handling.

The package installs `fcitx5.input-method.json` under
`/usr/share/myna/input-methods/`. Its `protocol`, `socket` and display fields
let Myna Settings list installed extensions without loading extension code.

The addon also reports the character before the cursor at acquire, so the
controller keeps its dictation-spacing behavior. It does not advertise secure
field detection because Fcitx5 does not expose that answer uniformly here.

## Build

```sh
cmake -S input-methods/fcitx5 -B build/input-methods/fcitx5
cmake --build build/input-methods/fcitx5
sudo cmake --install build/input-methods/fcitx5
```

Restart Fcitx5 after installation:

```sh
fcitx5 -r
```

For a temporarily staged build, point Fcitx5 at the addon files without
installing them:

```sh
FCITX_DATA_HOME=/path/to/stage/share/fcitx5 \
FCITX_ADDON_DIRS=/path/to/stage/lib/fcitx5:/usr/lib/x86_64-linux-gnu/fcitx5 \
fcitx5 -d --replace
```

The addon normally listens at
`$XDG_RUNTIME_DIR/snap.myna/myna-input-methods/fcitx5.sock`, the provider
directory in the Myna snap's runtime namespace. Set `MYNA_INPUT_METHOD_DIR`
on both Fcitx5 and Myna for another directory, or
`MYNA_INPUT_METHOD_SOCKET` for one exact socket path.

## Protocol

Each utterance uses one socket connection. Acquire replies, commits, preedits
and errors carry a big-endian `u32` byte length; every other frame is one byte:

```text
Myna -> addon: H
               A
               C <u32 length> <UTF-8>
               P <u32 length> <UTF-8>
               R

addon -> Myna: V <u32 version>
               Y <u32 length> <capabilities>
               K
               N
               S
               F
               G
               E <u32 length> <UTF-8>
```

`H/A/C/P/R` are hello, acquire, commit, preedit and release. `V` reports the
protocol version, and `Y/K` acknowledge acquire and commit/preedit; the acquire
payload starts with a capability byte and can
append the character before the cursor. `N` means no editable target, `F`
focus lost, `G` target gone and `E` a protocol error.
