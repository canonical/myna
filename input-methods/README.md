# Input-method extensions

An input-method extension lets Myna commit dictation through a desktop input
framework whose native API is not IBus. The Myna daemon only knows the generic
provider protocol; framework-specific code belongs in an extension package.

## Package layout

An extension installs:

- `/usr/share/myna/input-methods/<id>.input-method.json`, describing the
  extension without executing its code;
- any framework-specific files, such as a Fcitx5 addon;
- a running provider that creates
  `$XDG_RUNTIME_DIR/myna-input-methods/<id>.sock` as seen from Myna's snap
  runtime namespace. The unconfined provider publishes under
  `$XDG_RUNTIME_DIR/snap.myna/myna-input-methods/<id>.sock`.

The manifest is small enough for Myna Settings to list extensions read-only:

```json
{
  "id": "fcitx5",
  "name": "Fcitx 5",
  "protocol": 1,
  "socket": "fcitx5.sock"
}
```

## Provider contract

The provider is discovered by its socket, not by an input-framework process
name or toolkit environment variable. A connection starts with a protocol
version handshake. Protocol 1 supports acquire, commit, preedit, release,
target/focus loss and the character before the cursor. Secure-field detection
remains unknown until a framework can answer it consistently.

See `fcitx5/README.md` for the wire format and the first implementation.

## Trust

An input-method extension can observe focus changes and insert text. A future
installer must therefore install signed packages from a trusted source, show
the requested extension and its publisher, and require explicit enablement.
The first provider implementation is shipped by `myna-config`; it does not
introduce arbitrary code download or execution.
