# Repository and Module Layout

**Date:** 2026-06-12
**Status:** Accepted
**Authors:** Claude, with Charles

## Context

Myna spans two deliverables that share one protocol: the candidate-adapter
evaluation testbed (Taipei lab, phases 0–4) and the Ubuntu Desktop dictation
client (UD129). Both speak the IE114-shaped session contract, whose transport
and event vocabulary are still in flux. We need a layout that lets the testbed
move fast now without painting the desktop client into a corner, and that keeps
the in-flux pieces behind seams.

## Decision

A single `uv`-managed Python package, `src/myna`, with three subpackages and a
strict dependency direction:

```
myna.core     <- shared vocabulary; depends on nothing else in myna
myna.testbed  -> depends only on myna.core
myna.desktop  -> depends only on myna.core
```

- `myna.core` holds audio types (`AudioFormat`, `PcmChunk`, `AudioSource`),
  the provisional event vocabulary (`events.py`), session config, and the
  transport abstraction (`SttClient`/`SttSession`/`SttService` protocols plus
  the in-process `LoopbackClient`).
- `myna.testbed` holds `Candidate`/`Adapter`, the permanent `FakeAdapter`
  regression fixture, audio sources, and the `Harness` with its
  `ResultRecord`/metrics schema.
- `myna.desktop` holds UD129-side contracts (`TextInjector`, dictation state
  model). Implementation is a later workstream; stubs exist so both halves
  ideate against the same vocabulary.

Interfaces are `typing.Protocol`s, not ABCs: adapters and backends are
structural plug-ins and should not need to import a base class to conform.

Runtime dependencies are zero for Phase 0; model/engine dependencies (faster-
whisper, vLLM, NeMo) will be `uv` optional-dependency extras scoped per
adapter, never imported by `myna.core` or the harness.

## Rationale

Alternatives considered:

- **`uv` workspace with separate packages per deliverable** — right shape if
  the testbed ships independently, but premature while interfaces churn
  daily; every cross-package change would need lockstep version bumps. The
  subpackage boundaries preserve the option.
- **Separate repos for testbed and desktop client** — maximizes drift risk on
  exactly the thing that must not drift (the event/session contract), for no
  benefit at this stage.
- **ABC base classes instead of Protocols** — forces adapters to depend on
  myna at import time; Protocols keep candidates' messy environments (vLLM,
  NeMo) decoupled and make conformance checkable structurally.

## Consequences

- The harness can never grow a model-specific code path: it only sees
  `myna.core` types. Candidate messiness is forced into adapters by the
  import graph, not just by convention.
- The desktop client and testbed cannot drift apart on event semantics —
  there is one `events.py`.
- If the testbed later needs to be deployable standalone (Taipei lab), the
  package can be split into a `uv` workspace along the existing subpackage
  boundaries without rework.
