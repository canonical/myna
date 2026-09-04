# Language policy — scoped Python justification (UbuSTT / myna)

**Date:** 2026-06-17
**Status:** Proposed — decision requested from the language-policy owner
**Authors:** Claude, with Charles
**Audience:** Canonical language-policy owner(s); inference-snaps / platform team

## The ask, in one paragraph

UbuSTT (myna) is a local speech-to-text service. We request a **scoped**
exception to the Go/Rust default: keep **Python** for (a) the **model-execution
adapters** that drive vendored ML runtimes, and (b) the **evaluation/testbed
harness** (internal research tooling, not shipped to users). In exchange we
**commit to author the shipped, network-facing surface — transport, access
control, session lifecycle, and the desktop client — in Go or Rust.** We are
*not* asking to ship a network-facing Python server. The justification is
deliberately narrow: it covers only the code where Python is *forced* (the ML
runtime) or *unexposed and unshipped* (eval tooling).

## Why this is unavoidable, not a preference

We intend to ship Nemotron/FastConformer (NVIDIA NeMo). **NeMo is PyTorch — pure
Python with no Go or Rust binding, and none is on any roadmap.** The same is true
of the broader ASR ecosystem (faster-whisper, transformers, vLLM): the field's
*lingua franca* is Python over C++. The model we ship in two years for
best-in-class accuracy will, in all likelihood, also arrive as Python.

The consequence is blunt: **shipping a state-of-the-art ASR model means Python
executes in the product, regardless of what language we author our own code in.**
There is no single-language Go/Rust stack that contains NeMo. The only real
variables are *how much* authored Python ships and *where* — and our answer is
"as little as possible, confined to the model runtime, never on the exposed
surface."

## The precedent already in the platform

The reference inference snaps Canonical ships today (`qwen3-snap`,
`gemma4-snap`) follow a clear pattern:

- `modelctl` (the IE108 CLI) — **Go**.
- The engine `server` — a **thin bash script** that `exec`s a **native compute
  binary**: `llama-server` (llama.cpp, **C++**).
- No Go/Rust reimplementation of the compute engine. llama.cpp is **vendored**.

So the platform already ships a large, non-Go/Rust compute engine as a vendored
dependency, with thin Go/shell glue around it. This draws the line we are
asking the policy to recognise:

- **Authored code** — what we write and maintain (transport, access control,
  lifecycle, client). Go/Rust. *This is what the policy exists to govern.*
- **Vendored runtime** — an upstream ML engine we package but do not author
  (NeMo, like llama.cpp). Whatever language upstream ships. *Not authored
  Python; not the policy's target.*

A reading of the policy as "no Python *bytecode* may execute in a shipped snap"
would forbid NeMo — and, by the same logic, would have forbidden the C++
`llama-server` the reference snaps already ship. We do not think that is the
intent. Confirming it is the decision we need.

## Scope table — component by component

| Component | Today | Disposition | Why |
|---|---|---|---|
| WebSocket transport (frame parsing) | Python (~695 LOC `core`) | **Migrate → Go/Rust** | Network-facing, parses untrusted input — exactly the memory-safety case the policy targets. |
| Access control (socket perms, polkit, client identity) | not yet built | **Author in Go/Rust** | Security-sensitive, exposed; greenfield, so no rewrite cost. |
| Session lifecycle / idle-unload / socket activation | Python (~293 LOC `server`) | **Migrate → Go/Rust** | Maintained product glue; thin; no ML dependency. Migrating it also lets a front-half own the socket and fully release GPU memory at idle (retires a blocker we currently have against `modelctl run`). |
| Desktop client (hotkey, IBus, PipeWire capture) | not yet built | **Author in Go/Rust** | Greenfield; born in the target language, not a rewrite. |
| **Model-execution adapters** (drive NeMo / faster-whisper) | Python | **Keep Python (permanent)** | Forced: the runtimes are Python. Minimal, isolated behind a process/IPC boundary, **not network-facing**. |
| **Eval / testbed harness** (corpus, WER/CER metrics, bench, matrix) | Python (~1528 LOC) | **Keep Python (permanent)** | Internal research tooling, **not shipped to users**; Python is the right tool and outside the policy's product scope. |
| Native runtimes already in-hand (Qwen3 pure-C `.so`) | C/ctypes | **Native, FFI from Go/Rust** | Already a native library; no Python needed — proves runtimes are switchable per family. |

Authored, shipped, exposed code that ends up Python after this: **none.** Authored
shipped Python that remains: only the minimal model-execution adapter, which is
not network-facing.

## The boundary that keeps it clean

The migration moves the language seam from *in-process* (today our adapter
`import`s torch in the same process as the server — you cannot call NeMo from
Rust here) to a *subprocess* seam:

```
  Go/Rust front-half                          Python (or native) model worker
  - IE115 WebSocket server          PCM   →   - NeMo / faster-whisper, or
  - access control (polkit)        events ←     native whisper.cpp / qwen-pure-C
  - lifecycle / socket activation             - minimal: PCM in, transcript out
  - spawns / supervises the worker             - no network, no access control
```

Everything the policy cares about — network-facing, security-sensitive, authored
— is in Go/Rust on the left. Python (when a runtime forces it) is a minimal,
non-network-facing worker on the right, behind an internal, versioned protocol.
This **generalises the platform's own pattern** — "engine" stops meaning
"native binary" and means "engine worker in whatever language the model forces";
llama.cpp is the C++ instance, NeMo the Python instance. It also hardens our
existing design invariant ("model messiness lives in adapters") from a class
boundary into a process boundary. Native runtimes (whisper.cpp, the Qwen pure-C
library) can be FFI-linked into the front-half directly where latency warrants.

## Sequencing (so this is not a stop-the-world rewrite)

1. **Now:** the wire-protocol/spec work (IE115 alignment, versioning) is
   language-agnostic — it proceeds regardless. Prototyping continues in Python;
   the policy binds at ship/maintain time, not prototype time.
2. **Productization (strangler, behind the existing wire contract):** author the
   greenfield exposed surface (access control, desktop client) in Go/Rust from
   day one; migrate the thin exposed layers (transport, lifecycle) once IE115 is
   settled. Contract tests over two transports already make the wire format
   swappable and verify equivalence.
3. **Permanent:** model-execution adapters and the eval harness stay Python.

## What we are explicitly *not* asking for

- Not asking to ship a network-facing Python server.
- Not asking to keep access control or transport in Python.
- Not asking to rewrite vendored upstream ML runtimes (NeMo, PyTorch) — that is
  neither feasible nor what the policy intends, per the llama.cpp precedent.

## Decision requested

Confirm the **authored-code vs vendored-runtime** reading: that the Go/Rust
default governs the code we author and maintain (which we will satisfy on the
entire exposed surface), and that **vendored ML runtimes plus their minimal,
non-exposed adapter, and internal eval tooling, may remain Python.**

If confirmed, the migration is bounded and we ship a compliant exposed surface
with a clear roadmap. If the reading is instead "no Python executes in a shipped
snap," then Nemotron — and any future Python-only SOTA model — cannot ship, and
we need that established now, before further work assumes otherwise.
