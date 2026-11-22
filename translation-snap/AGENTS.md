# Preface

Read this before changing the `myna-translation` snap, its language-pair components or the Bergamot runtime. The generic text snap is `myna-text` (`text-snap/AGENTS.md`).

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

One strictly confined snap, no `network` plug, serving `myna-text-v1` to the Myna daemon through the `inference-provider` content share with `MYNA_ROLE=text`, as its own share beside `myna-text` and the speech snap on `myna:backend` (`DECISIONS.md` 7.1). The models are components, one per directed language pair, found at run time by their `myna-text-model.json`. The Python daemon (`myna-translation`, `server/src/myna/translation/`) drives `myna-bergamot`, a C++ line-at-a-time front end built here from a pinned bergamot-translator without MKL. It shares the protocol, component scan and server loop with `myna-text` (`myna.text.daemon`) and carries no onnxruntime.

# Important

- The wire is defined by `client/myna-desktop/src/process/wire.rs`; `server/tests/text/test_protocol.py` reads that file.
- Never log or return dictated text: ops, model ids, durations and outcomes only. Errors name a reason, not the input.
- Never hand-edit `models/*/myna-text-model.json`, `models/*/NOTICE`, `catalog.json` or the marked blocks of `snap/snapcraft.yaml`: edit `plugins/*.toml` and run `dev/gen-plugins.py .` (`make lint-plugins` fails on drift).
- The declared `latency_ms` and `quality` come from `plugins/*.measured.json`. They are measurements (see `README.md`), re-measure before changing them.
- Every build input is pinned: the bergamot commit (its submodules ride on it), the Python wheels by hash (`dev/prepare.sh` exports `server/uv.lock`), the translation files by sha256. Moving one is a commit.
- The MPL-2.0 attribution (Mozilla, source link, unmodified-file hashes) and the bergamot link-set licence texts live here; `dev/build-bergamot.sh` fails if a licence text is missing.
- Pack with `umask 022`: a 077 shell leaves files in local snaps unreadable to the daemon.
- No `hardware-observe` plug: without onnxruntime nothing reads `/sys/bus/pci`.
- Publishing is Charles' call (`plugin-store-enablement`); everything is sideloaded until then.

# Directory

- `snap/snapcraft.yaml` - The snap and its components.
- `bergamot/` - `myna-bergamot.cpp`, the line-protocol front end.
- `models/` - Per-component manifest, NOTICE and the Marian config.
- `dev/` - `prepare.sh`, `download-models.sh` (a thin call of `../dev/stage-plugins.sh`), `build-bergamot.sh`, `prune-runtime.sh`, `smoke.py`.
- `NOTICE` - Third-party notices the snap ships with.

# Documents

- `README.md` - What the snap serves, how to build and install it, the translation runtime.
- `../.kb/text-plugin-contract.md` - The manifest and capabilities contract this snap's models follow.
