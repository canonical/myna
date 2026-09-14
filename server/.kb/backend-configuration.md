# Preface

Read this document when adding backend settings, model or engine selection, provisioning, residency policy, or a configuration UI.

Read the top-level `.kb/agents.md` file before continuing below.

# Architecture

Keep three control layers separate:

1. Provisioning installs snaps and model/runtime components through snapd.
2. Persistent backend configuration selects engines, models, residency, and adapter tuning through each snap's `modelctl` surface.
3. Session parameters such as language and prompt apply to one transcription session.

The transcription socket is an unprivileged data plane, not a privileged configuration API. A session model request tests compatibility with the running server; it does not switch the installed backend model.

Inference backends communicate over a Unix socket shared through a content slot with content id `inference-provider`, per the IE115 spec. The shared directory holds the socket and a `provider.env` naming `SNAP_NAME`, `SNAP_INSTANCE_NAME` and `UNIX_SOCKET` (the socket's name relative to that directory); `myna-server --share-provider` is its only writer, atomically and before it binds, and consumers iterate every shared directory rather than trusting its name. TCP is not the product transport, so Myna neither writes nor reads `OPENAI_BASE_URL`. Configuration reads should remain unprivileged; mutations that require privilege belong behind snapd authorization.

Engine auto-selection matches hardware capabilities, not free memory capacity. Manual engine selection is a supported diagnostic and expert override. Failure to load a model must be surfaced explicitly rather than hidden by fallback.

# Important

- Do not add persistent mutations to the transcription protocol.
- Do not conflate installing a component, selecting the active model, and requesting a model for one session.
- Keep the CLI as the authoritative current control surface until a machine-readable configuration schema exists.
- Expose residency to product UI as user intent; keep raw timing knobs available to operators.
