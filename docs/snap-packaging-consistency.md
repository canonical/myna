# Inference snap packaging consistency

The seven inference snaps (whisper, parakeet, sherpa, myna-funasr, qwen,
nemotron, audio8) are siblings: same modelctl CLI, same UbuSTT socket, same
engine/runtime/model manifest layout. Drift between them is invisible until
something installs one on a clean machine and it fails.

That is not hypothetical. Two snaps had silently diverged, and the symptom was
a benchmark run collapsing halfway through rather than any test going red.

`server/tests/test_snap_packaging.py` now asserts the invariants below directly
against each `snapcraft.yaml`. It runs in ~0.3 s with no VM and no build, and
it is the cheapest place to notice a snap has drifted. Manifest *schema*
validity (runtime `name`, model identifiers, capability vocabulary, component
cross-checks) is asserted separately by `dev/lint-packages.sh`, which runs
modelctl's own `debug lint-package` over every snap.

## The invariants

| Invariant | Why it matters | Caught |
| --- | --- | --- |
| All snaps pin the same modelctl release | Manifest semantics move with the CLI (runtime `name`, model identifiers, status entrypoints); a drifted snap breaks silently. | - |
| No part stages `pciutils` | Since v2.0.0-beta.6 modelctl uses lscompute and reads `/sys` directly; the lspci binary is dead weight that looks load-bearing. | - |
| CLI app plugs `hardware-observe` | lscompute still cannot read `/sys` under strict confinement without it. | parakeet, sherpa |
| Install hook plugs `hardware-observe` | The hook selects the engine at install and needs the same access. | parakeet, sherpa |
| Install hook runs `use-engine` | With no active engine, `show-engine` / `list-models` / `status` all fail and the daemon exits 1 on every start. | parakeet, sherpa |
| Install hook sets `ws.unix-socket`; engine scripts read it | Since v2.0.0-beta.12 `modelctl status` builds the entrypoint of a ws+unix server from that key; the retired `socket.path` was invisible to status. | - |
| No app plugs `network` | Every snap's own header states weights ship as components and there is no runtime download. | qwen, nemotron |
| `snap/hooks/` holds only hooks | It is a namespace snapd scans, not a scratch directory. | sherpa (a stray copy of `dev/prepare.sh`) |
| `ubustt-socket` slot exposed | Confined clients reach the backend over the content share. | - |
| Every `engines/<name>/` has `server` + `engine.yaml` | A half-declared engine fails at run time, not at pack time. | - |
| No engine script hardcodes `--streaming` | Emission mode is a user-facing config toggle; a baked-in flag is not a choice. | parakeet, sherpa |

## Differences that are deliberate, not drift

Uniformity is not the goal - working snaps are. These asymmetries are correct
and the tests permit them:

- **parakeet and sherpa ship one engine and their `server.sh` execs
  `engines/cpu/server` directly**, instead of asking modelctl to score hardware
  to discover the only possible answer. Their install hooks therefore activate
  the engine *by name* rather than with `--auto`, which keeps the modelctl
  surface working even where `hardware-observe` is unavailable.
- **Their daemon app does not plug `hardware-observe`/`opengl`**, unlike the
  others. It never touches hardware detection - the engine script reads config
  only - so the plugs would be unused privilege.
- **`network-bind` on every daemon** is required, not a network grant: snapd's
  seccomp filter gates `listen()` behind it even for a Unix domain socket.
- **Streaming defaults differ per snap.** parakeet and sherpa default
  `streaming=true` because progressive commit is their reason to exist; whisper
  and nemotron default `false`. A 480 ms transducer defaulting to batch would
  be a worse product, so the default follows the model, not a house style.

## What the static tests cannot catch

They read `snapcraft.yaml`. They do not prove the built snap works. Two things
only a real install shows, and `tests/spread/adapter-smoke/` covers both:

- `snap install --dangerous` carries no snap declaration, so manual-connect
  plugs stay unconnected and the install hook's engine selection is skipped
  entirely. Anything sideloading these snaps (the benchmark runner, a developer,
  CI) has to connect, select, and restart by hand.
- Whether the `streaming` config key actually reaches the adapter. The smoke
  test asserts committed segments appear in streaming mode and do not in batch,
  which is the only evidence the toggle is not being silently ignored.
