# Implementation Plan: Myna orchestrator snap

**Branch**: `005-myna-orchestrator-snap` | **Date**: 2026-07-22 | **Spec**: [spec.md](spec.md)

## Summary

Package the Rust dictation client as the strictly-confined `myna` snap, and
make the backend session socket reachable from confinement via a writable
content share on each inference snap (the T14c "content interface" option —
snapd's content interface explicitly supports named sockets in writable
shares). Three small confinement bugs in `myna-desktop` are fixed with unit
tests (IBus discovery across the snap-private `$HOME`, the control-socket
path under `$XDG_RUNTIME_DIR`, revisioned shortcut commands). A GitHub
workflow builds + smoke-tests the snap.

## Technical Context

**Language/Version**: Rust 1.75+ (client), snapcraft 9 YAML, GHA workflows
**Primary Dependencies**: snapd ≥ 2.75 (`pipewire` interface, writable
content shares), gnome-46-2404 platform snap (GTK4 indicator), LXD builds
**Storage**: n/a (no persistence — privacy invariant)
**Testing**: `cargo test --workspace` (unit, hermetic) + live on-machine
verification (confined install, content share, portals, IBus, D-Bus)
**Target Platform**: Ubuntu 26.10 amd64, GNOME/Wayland, snapd 2.75.2
**Project Type**: snap packaging + small Rust client fixes
**Constraints**: no `network` plug (offline); no audio persistence; strict
confinement; `pipewire` is deny-auto-connection upstream (manual connect is
documented, not a bug)

## Constitution Check

- I (TDD): the three confinement fixes land with unit tests first/together;
  packaging itself is verified by build + live runs (evaluation of a
  packaging artifact, not library logic). PASS.
- II (integration-readiness): the snap is verified against the real
  `whisper` snap socket on a live GNOME session. PASS.
- III (performance watermarks): unchanged hot path; packaging adds no
  per-frame work. PASS (no new watermark needed).
- IV (Workshop dev env): snap builds stay outside Workshop for now —
  recorded under T55(c); CI uses snapcraft directly. NOTED.
- V (privacy/offline): no `network` plug; no audio persistence; the D-Bus
  slot carries state+level only (feature-004 contract). PASS.

## Project Structure

```text
myna-snap/                    # new snap project (mirrors whisper-snap layout)
  snap/snapcraft.yaml         # apps/plugs/slot/parts
  scripts/myna-daemon         # daemon launcher (portal + --dbus defaults)
  scripts/myna-install-shortcut  # dconf-based shortcut binding
  dev/prepare.sh              # rsync client/ into the project (craft-parts rule)
specs/005-myna-orchestrator-snap/{spec,plan,tasks}.md
.github/workflows/snap.yml    # build + confined smoke test
client/myna-desktop/src/      # 3 confinement fixes (+tests)
whisper-snap|nemotron-snap|qwen-snap/snap/snapcraft.yaml  # ubustt-socket slot
```

## Interface design (the load-bearing decisions)

| Boundary | Mechanism | Why |
|---|---|---|
| Mic capture | `pipewire` plug | snapd 2.75 grants `/run/user/*/pipewire-0`; `audio-record` is the PulseAudio-shaped legacy plug and the native backend doesn't use it. deny-auto-connection upstream → documented manual connect. |
| Backend socket | writable content share (`source: write`) | snapd adds AppArmor `mrwklix` on the shared dir *precisely* so named sockets work (content.go comment). `system-files` would be super-privileged + store-review; polkit/identity stays T17. |
| Text injection | `desktop-legacy` | carries the IBus daemon socket rules (`~/.cache/ibus/dbus-*`, address file under `~/.config/ibus/bus`). |
| Hotkey | `desktop` → GlobalShortcuts portal | portals only serve packaged apps — the README's "packaged only" path; control-socket mode kept as fallback. |
| Indicator | `com.canonical.Myna.Dictation` dbus slot + `desktop` notifications | confined name ownership needs the slot declaration. |
| GTK overlay | `gnome` extension (gnome-46-2404) | standard core24 GTK4 story; overlay stays opt-in (`--overlay`). |

## Verification plan (tonight, this machine)

1. `cargo test --workspace` + clippy.
2. `snapcraft pack` myna-snap; `snap install --dangerous`.
3. Rebuild + reinstall `whisper` with the slot; connect
   `myna:backend ← myna-whisper:ubustt-socket`, `myna:pipewire`.
4. Confined round-trip: `myna.testbed --socket …/ubustt.sock --clip <wav>`.
5. Daemon: `--dbus` name ownership on the bus; portal bind attempted;
   control mode + `myna.toggle` toggle cycle.
6. CI workflow green (static inspection + act-less review; first real run on
   push).

## Deferred (recorded, not done tonight)

- T17: identity-based access control on the socket (polkit) — the share is
  "any snap an admin connects".
- T48: multi-backend discovery; the plug connects one backend at a time.
- Autostart on login (user daemons are still experimental in snapd; a
  `.desktop`/Startup-Applications story is a follow-up).
- Store registration of the name `myna` (verified unregistered tonight).
- Rebuilding nemotron/qwen snaps with the slot (yamls changed; whisper is
  the verified reference).
