# Preface

Read this before changing the `myna-text` snap, its model components or the `myna-text-v1` provider in `server/src/myna/text/`.

Read the top-level `.kb/agents.md` file before continuing below.

# Overview

One strictly confined snap, no `network` plug, serving `myna-text-v1` to the Myna daemon through the `inference-provider` content share with `MYNA_ROLE=text`. Unlike the speech snaps it has no modelctl, engines or runtimes manifests: the models are components found at run time by their `myna-text-model.json`. The Python daemon (`myna-text`) runs the disfluency tagger on onnxruntime and drives `myna-bergamot`, a C++ line-at-a-time front end built here from a pinned bergamot-translator without MKL.

# Important

- The wire is defined by `client/myna-desktop/src/process/wire.rs`; `server/tests/text/test_protocol.py` reads that file, so a field added on either side fails a test rather than a user's dictation.
- Never log or return dictated text: ops, model ids, durations and outcomes only. Errors name a reason, not the input.
- The component manifests (`models/*/myna-text-model.json`) hold the declared `latency_ms` and `quality`. They are measurements (see `README.md`), re-measure before changing them.
- Every build input is pinned: the bergamot commit (its submodules ride on it), the Python wheels by hash (`dev/prepare.sh` exports `server/uv.lock`), the translation files by sha256, the tagger by sha256. Moving one is a commit.
- Pack with `umask 022`: a 077 shell leaves files in local snaps unreadable to the daemon.
- `MYNA_TAGGER_SRC` is the only way to stage the tagger until it is published; publishing it, and the snap, is Charles' call.

# Directory

- `snap/snapcraft.yaml` - The snap and its two components.
- `bergamot/` - `myna-bergamot.cpp`, the line-protocol front end.
- `models/` - Per-component manifest, NOTICE and, for translation, the Marian config.
- `dev/` - `prepare.sh`, `download-models.sh`, `build-bergamot.sh`, `prune-runtime.sh`.
- `NOTICE` - Third-party notices the snap ships with.

# Documents

- `README.md` - What the snap serves, how to build and install it, the translation runtime.
