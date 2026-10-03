<h1>
  <img src="client/data/icons/hicolor/scalable/apps/com.canonical.Myna.Config.svg" width="70" align="absmiddle" alt="Myna Logo">
  Myna 
</h1>

> [!WARNING]
> Myna and several of its features are in heavy development. We are actively
> refining our core concepts, protocols, and specifications, and will likely
> introduce major breaking changes prior to a stable release. Early testing
> and feedback is highly appreciated!

Offline speech-to-text for Ubuntu Desktop. Press a key, speak, and the
transcript is injected into the focused application. Nothing leaves the
machine: inference runs in strictly confined local models deployed as snaps.

The project is named for the [Myna](https://en.wikipedia.org/wiki/Myna), a
bird that listens to and reproduces human speech with striking clarity.

## Install

There's a GUI installer intended for end-users. It's available in a PPA,

```
# TODO: publish to the Ubuntu archive when stable, or a Canonical namespace at least!
sudo add-apt-repository ppa:charles05/myna-config
sudo apt install myna-config
```

To build the latest **package** locally, run `make build-deb` followed by `sudo apt install ./target/deb/myna-config_*.deb`.

To build the latest **binary** locally,

```
# edit ...
workshop exec myna -- bash -lc 'cd /project/client && cargo build --release --locked'
./client/target/release/myna-config
```

## Develop

This repository is written to be explored with a coding agent. Point one at
the tree and ask. [`AGENTS.md`](AGENTS.md) is the entry point: it lays out the
architecture and indexes the `.kb/` knowledge documents, and every subsystem
carries its own `AGENTS.md`.

The manual development loop is,

```
# make changes ...
# start a backend
(cd server && uv run myna-server --adapter whisper --model base --socket /tmp/myna.sock) &
# start a client
cd client && cargo run --bin myna-testbed -- --socket /tmp/myna.sock --mic
# repeat ...
```

For text injected into the focused app, run the `myna-desktop` daemon instead; see [`client/README.md`](client/README.md).


## Acknowledgements

- Antonio Zugaldia of [SpeedOfSound](https://www.speedofsound.io/), for early
  design discussions and advice.
- Kieirra, author of [Murmure](https://github.com/Kieirra/murmure), for their
  advice on GitHub Discussions and for the ideas and code Myna ported: the
  Parakeet backend's chunked-commit loop and adaptive VAD follow Murmure's, and
  the parakeet snap ships Murmure's int8 ONNX bundle.

## License

AGPL-3.0-or-later. See [`LICENSE`](LICENSE).
