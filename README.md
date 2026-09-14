# Myna

Offline speech-to-text for Ubuntu Desktop. Press a key, speak, and the
transcript is injected into the focused application. Nothing leaves the
machine: inference runs in strictly confined snaps that receive audio over a
Unix socket, and no audio is ever persisted.

The project is named for the [myna](https://en.wikipedia.org/wiki/Myna), a
bird that listens to and reproduces human speech with striking clarity.

## Install

Myna is a client snap plus one inference snap per model family.
[`myna-snap/README.md`](myna-snap/README.md) is the setup path; pick a backend
from the `*-snap/` directories, with whisper as the reference. Myna Settings,
the configuration app, ships as a deb:
[`myna-config-deb/README.md`](myna-config-deb/README.md).

## Develop

This repository is written to be explored with a coding agent. Point one at
the tree and ask. [`AGENTS.md`](AGENTS.md) is the entry point: it lays out the
architecture and indexes the `.kb/` knowledge documents, and every subsystem
carries its own `AGENTS.md`.

- The toolchain is a [Canonical Workshop](https://ubuntu.com/workshop/docs)
  definition in `.workshop/`. Run `workshop launch myna` once; every gate then
  runs inside it.
- `make help` lists the gates, suites, coverage, snap and benchmark targets.
  `make preflight` is the merge bar, and [`.kb/verification.md`](.kb/verification.md)
  defines done.
- [`client/README.md`](client/README.md) runs the dictation client from source
  against a `myna-server` started from `server/`.
- Features are designed under `specs/` before implementation, governed by the
  [constitution](.specify/memory/constitution.md). Merged specs are historical
  snapshots, not live documentation.

## Acknowledgements

- Antonio Zugaldia of [SpeedOfSound](https://www.speedofsound.io/), for early
  design discussions and advice.
- Kieirra, author of [Murmure](https://github.com/Kieirra/murmure), for their
  advice on GitHub Discussions and for the ideas and code Myna ported: the
  Parakeet backend's chunked-commit loop and adaptive VAD follow Murmure's, and
  the parakeet snap ships Murmure's int8 ONNX bundle.

## License

AGPL-3.0-or-later. See [`LICENSE`](LICENSE).
