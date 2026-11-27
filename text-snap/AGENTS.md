# Preface

Read this before changing the `myna-text` snap, its model components or the `myna-text-v1` provider in `server/src/myna/text/`. Translation is the sibling `myna-translation` snap (`translation-snap/AGENTS.md`).

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

One strictly confined snap, no `network` plug, serving `myna-text-v1` to the Myna daemon through the `inference-provider` content share with `MYNA_ROLE=text`. Unlike the speech snaps it has no modelctl, engines or runtimes manifests: the models are components found at run time by their `myna-text-model.json`. The Python daemon (`myna-text`) runs the disfluency tagger on onnxruntime. The protocol, component scan and server loop are shared with `myna-translation` (`myna.text.daemon`); this snap registers only the tagger kind and carries no bergamot code.

# Important

- The wire is defined by `client/myna-desktop/src/process/wire.rs`; `server/tests/text/test_protocol.py` reads that file, so a field added on either side fails a test rather than a user's dictation.
- Never log or return dictated text: ops, model ids, durations and outcomes only. Errors name a reason, not the input.
- Never hand-edit `models/*/myna-text-model.json`, `models/*/NOTICE`, `catalog.json` or the marked blocks of `snap/snapcraft.yaml`: edit `plugins/*.toml` and run `dev/gen-plugins.py .` (`make lint-plugins` fails on drift).
- The declared `latency_ms` and `quality` come from `plugins/*.measured.json`. They are measurements (see `README.md`), re-measure before changing them.
- Every build input is pinned: the Python wheels by hash (`dev/prepare.sh` exports `server/uv.lock`) and the tagger by sha256. Moving one is a commit.
- English is served by `model-disfluency-en` (`myna-disfluency-en`, the English-only tagger), French, German and Spanish by `model-disfluency-fr-de-es` (`myna-disfluency`, the multilingual tagger). The multilingual model reads English too, but on real dictations it deleted more content there, numbers included (`server/.kb/text-processing-evaluation.md`); do not add `en` to its `languages` without a real-dictation measurement that beats the English one. The `model-disfluency-fr-de-es` name must be registered on the store before its first upload.
- Pack with `umask 022`: a 077 shell leaves files in local snaps unreadable to the daemon.
- `MYNA_TAGGER_EN_SRC` and `MYNA_TAGGER_SRC` are the only way to stage the taggers until they are published; publishing them, and the snap, is Charles' call.

# Directory

- `snap/snapcraft.yaml` - The snap and its components.
- `plugins/` - One `<name>.toml` per component, the only hand-edited source (plus `<name>.measured.json`); `dev/gen-plugins.py` generates from it the `components:` stanza, `models/`, and `catalog.json`.
- `models/` - Per-component manifest and NOTICE, generated.
- `catalog.json` - Generated catalog of this snap's plugins, shipped in the snap.
- `dev/` - `prepare.sh`, `download-models.sh` (a thin call of `../dev/stage-plugins.sh`), `prune-runtime.sh`, `smoke.py`.
- `NOTICE` - Third-party notices the snap ships with.

# Documents

- `README.md` - What the snap serves, how to build and install it.
- `../.kb/text-plugin-contract.md` - The manifest and capabilities contract this snap's models follow.
