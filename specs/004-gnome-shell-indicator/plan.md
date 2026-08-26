# Implementation Plan: GNOME Shell Extension for Myna Dictation UI

**Branch**: `004-gnome-shell-indicator` | **Date**: 2026-07-21 (HUD redesign: 2026-07-30; wave-ribbon: 2026-07-30; architecture revision: 2026-08-26) | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/004-gnome-shell-indicator/spec.md`

## Summary

**(2026-08-26 architecture revision — this supersedes the delivery shape
below; the two-deliverable split becomes three, and the extension's role
inverts from renderer to host. Kept for context; read this section together
with the revision paragraphs.)**

Deliver a **GNOME Shell extension** (GJS, in the compositor) that renders a
focus-safe dictation **HUD pill** — bottom-center of the screen, styled like
GNOME's own volume/brightness OSD, showing a flowing, accent-colored **wave
ribbon** audio-level meter (unfolds on start, flows while speaking, relaxes on
pause, morphs on stop — with a reduced-motion fallback), a content-free
status label, and a mic/mic-slash icon — plus an optional panel toggle. On
GNOME/Wayland a normal client cannot show an always-on-top, non-focus-stealing
overlay (survey in `docs/desktop-injection.md` §2); running inside Mutter is
the GNOME-blessed fix. **(2026-07-30 revision: the original "goop" ribbon/blob
design — see R6/R12 in research.md — is replaced by this bottom-center HUD
pill; the `RibbonView` implementation is deleted, not kept as an alternate.)**
**(2026-07-30 wave-ribbon revision: the subsequent segmented bar meter — R16 —
is itself replaced by the flowing wave ribbon described above — R17 — colored
from the desktop's accent-color preference with a reduced-motion fallback,
R18/R19; a non-shipped standalone `dev-lab` tuning tool accompanies it, R20.)**

**2026-08-26 architecture revision** (research R21–R27): the HUD pill's
*drawing* moves out of the compositor into a standalone **Rust GTK4 +
libadwaita application** — new workspace crate `client/myna-hud`, binary
`myna-hud` — rendering the same bottom-center pill with the wave ribbon via
the **GPU/GLSL path only** (Cairo and the Shell-50 fallback are deleted).
The GNOME Shell extension remains, inverted into a **thin overlay host**:
it spawns the binary through `Meta.WaylandClient` (so it structurally knows
its child's window), adopts that window as a dock-typed, window-list-hidden,
all-workspaces, always-above, never-focusable overlay, positions it
bottom-center, supervises the process (bounded-backoff respawn), and owns the
member-less **`org.myna.Shell` presence name**. The renderer application
becomes the consumer of the existing `org.myna.Dictation` contract (unchanged
wire), reads accent color via libadwaita's color manager and reduced motion
via GTK's absent-safe `gtk-interface-reduced-motion` (never a direct read of
the new a11y key — crash guard), and doubles as the developer lab
(`--lab`, no backend required) and backend simulator (`--serve-dbus`),
replacing the GJS `dev-lab/` and Python `dev-lab-gpu/` tools (deleted).
`myna-desktop` drops its experimental `ui-gtk` overlay and gains the
presence-driven launcher policy (suppress the notification fallback while the
host is up; contract-only seam for a future non-GNOME layer-shell backend).
`myna-hud` ships in the myna snap (snapcraft `gnome` extension — the
~13 MB GTK staging cost is accepted, inverting T69's demotion which was about
a one-label window, not the shipped renderer), exposed as the well-known
`/snap/bin/myna-hud`.

Three deliverables, two contracts between them:
1. **`myna-hud` renderer application** (Rust, TDD, shipped component — the
   *new* logic-bearing half): pure modules ported 1:1 from the extension
   (state mapping, VU envelope, ribbon model + GLSL generator, HUD logic,
   position math is host-side), the GTK window/pill UI with a `GLArea`
   GPU renderer (dual GL profile), the `org.myna.Dictation` consumer
   (name-watch + PropertiesChanged, injectable proxy seam), per-state input
   region (empty; dismiss rect during critical error), accent/reduced-motion
   tracking, and the `--lab`/`--serve-dbus` modes.
2. **GNOME Shell extension — thin host** (GJS, harness-tier, much smaller
   than before): launch (`Meta.WaylandClient`), adopt + overlay-typing
   (DOCK/hide-from-window-list/stick/above), position + reposition
   (anti-feedback), supervision (respawn/terminate), presence name
   (`org.myna.Shell`). No drawing, no dictation data, no `St`/`Clutter`
   rendering modules.
3. **`myna-desktop` D-Bus publisher + launcher policy** (Rust, TDD, shipped
   — existing): unchanged `DbusIndicator`/`DbusTrigger`/level-pump contract;
   adds the presence watch and fallback suppression; removes
   `ui-gtk`/`GtkIndicator`.

The indicator remains **pure UI**: state + levels over `org.myna.Dictation`,
never transcript; IBus injection stays in `myna-desktop` (feature 003). The
recoverable/critical severity split (R13, 2026-07-30) is unchanged by this
revision — the renderer inherits both tiers verbatim. On GNOME the hosted
renderer is the preferred indicator surface — `NotifyIndicator` remains the
fallback when the extension is absent (selected via the presence name,
FR-017a/FR-023).


## Technical Context

**(2026-08-26 revision)**: language/dependency/testing rows below are
rewritten for the three-deliverable shape; the original two-deliverable rows
are superseded (publisher facts survive — see `contracts/publisher.md`).

**Language/Version**:
- `myna-hud` renderer: **Rust** (stable, workspace edition 2021,
  `rust-version = 1.75`), GTK4 + libadwaita via the gtk-rs bindings
  (`gtk4` 0.11 `v4_10`, `libadwaita` 0.9 — **no version-gated features**:
  the newer surfaces are read dynamically at runtime via GObject property
  lookup, which needs no cargo feature and no per-environment build matrix;
  see Primary Dependencies).
- Extension host: **GJS**, GNOME Shell 50 and 51, using the public extension
  API plus mutter's introspected `Meta.WaylandClient`/`Meta.Window` APIs
  (verified present in both `Meta-18.gir` (mutter 50) and `Meta-51.gir`
  (mutter 51)).
- `myna-desktop` publisher/policy: Rust (unchanged).

**Primary Dependencies**:
- Renderer (`client/myna-hud`, new crate): `gtk4` (GLArea, settings),
  `libadwaita` (application/style manager/color), `gl` (GL **types and
  constants**; the T101 spike found Ubuntu's libepoxy exports no generic
  `epoxy_get_proc_address`, only per-function dispatch *pointers* — so the
  renderer declares those `epoxy_gl*` symbols as extern statics, which is
  what epoxy's own C headers dereference and what auto-selects GL vs GLES
  per context, rather than using a `load_with` loader), `zbus`
  (consumer proxy — already vendored family), `gettext-rs` (domain `myna`,
  `gettext-system`), plus a tiny surfaceless-EGL check dependency behind an
  env-gated test feature. No network, no audio.
  **Version matrix (2026-08-26, corrected against the snap's SDK)**: the
  packaged binary builds and runs against the
  [gnome-46-2404 SDK](https://github.com/ubuntu/gnome-sdk/blob/gnome-46-2404-sdk/snapcraft.yaml)
  (GTK 4.18.6, libadwaita 1.7.7 — Ubuntu-patched to expose the **Yaru accent
  colors**), not the noble archive; the client workshop's ubuntu@24.04 base
  carries GTK 4.14/libadwaita 1.5; 26.04 dev hosts carry GTK 4.22/1.9.
  Therefore no `gtk4/v4_22`/`libadwaita/v1_7` compile-time features: the
  newer surfaces are consumed via runtime GObject property lookup —
  `gtk-interface-reduced-motion` (absent in the snap's GTK 4.18 → the
  `enable-animations` GSettings fallback is the shipping path there) and
  `AdwStyleManager:accent-color-rgba` (present in the snap's 1.7.7) read as
  a boxed `gdk::RGBA` rather than the `AdwAccentColor` enum, because
  Ubuntu's Yaru patches add enum values the upstream Rust enum does not know.
  An **optional cargo feature** (off for the 24.04 workshop, on for snap
  builds) was considered and rejected: runtime property lookup already
  covers every stack with zero per-environment build flags, and simply
  bumping the workshop base to 26.04 later removes even the fallback paths
  without any code change.
- Extension host: Shell platform modules only — `Meta` (WaylandClient,
  Window), `Gio`/`GLib` (presence name via `Gio.bus_own_name`; **no
  `org.myna.Dictation` proxy at all**). ESM modules; no bundler.
- Publisher: unchanged (`zbus` 5.x); the launcher policy adds a name-watch
  seam (zbus `SignalStream`/`NameOwnerChanged` or a `Bus` trait extension —
  fake for hermetic tests).
- Snap packaging: snapcraft `gnome` extension (stages GTK4 + libadwaita);
  `myna-hud` exposed as the `myna-hud` snap command (well-known
  `/snap/bin/myna-hud`).

**Storage**: N/A. No settings store in scope. The renderer keeps in-memory
transient state (current dictation state + last level); the host keeps
bookkeeping (client/window handles, respawn state); nothing persisted. Audio
is never touched by any deliverable.

**Testing**:
- Renderer (Rust, TDD, shipped tier): hermetic `cargo test -p myna-hud` over
  the ported pure modules (state mapping, envelope ballistics + stale decay,
  ribbon model phases, shader generator conformance — every `#define` matches
  the Rust tuning constants; uniform packing agrees with the model — port of
  `ribbonGlsl.test.js` — plus HUD logic and input-region geometry as pure
  functions), the D-Bus consumer lifecycle behind an injectable fake-proxy
  seam (port of `lifecycle.test.js`), and the simulator's state mapping.
  Env-gated (`MYNA_HUD_GL_TESTS=1`) headless EGL render check compiles the
  shader on a real driver and rasterizes non-blank/non-flooded per-phase
  distinct frames (port of `render_headless.py`; surfaceless EGL, no
  display). Manual on-hardware acceptance for the composited visuals.
- Extension host (GJS, harness-tier): pure placement math, binary resolution,
  respawn policy, adoption idempotence, and presence lifecycle as GJS
  contract tests (no Shell); a headless-Shell integration test (existing
  `entrance-visual.sh` harness pattern) driving a stub HUD window where the
  environment allows; manual on-hardware acceptance for live compositor
  behavior (focus safety, click-through, dock typing).
- Publisher (Rust, TDD): unchanged hermetic suites + new launcher-policy tests
  over a fake presence seam (P20–P23); `MYNA_DBUS_TESTS`-gated round-trips
  extended to cover C12/C13.
- CI/workshop: `make test-client` gains `myna-hud`; the `myna-shell`
  workshop's gjs suite shrinks to the host modules; new SDK deps (GTK4/libadwaita
  dev headers, `glslang-tools` for shader validation in tests) land in the
  Workshop definitions in the same PR (constitution IV).

**Target Platform**: Ubuntu Desktop 26.04+ on Wayland, GNOME Shell 50/51
(mutter ABI 18 / 51); session D-Bus present. The toolkit floor is
environment-dependent: the snap carries its own GTK 4.18/libadwaita 1.7.7
(gnome-46-2404 SDK), 26.04 hosts carry 4.22/1.9, and the 24.04 workshop
carries 4.14/1.5 — the renderer probes the newer surfaces at runtime and
degrades to the documented fallbacks, so no single floor applies (see
Primary Dependencies). Older GNOME and non-GNOME desktops are out
of scope (notification fallback; the layer-shell backend is contract-ready
follow-up).

**Project Type**: Desktop — a Rust workspace addition (the renderer, in the
new `client/myna-hud` crate; the publisher/policy already in
`client/myna-desktop`), plus the rewritten GJS extension bundle
`extensions/myna-shell/` (thin host), plus the snap packaging change.

**Performance Goals** (inherited from feature 003 / UD129, pinned as watermarks
— constitution III; unchanged by the revision except the renderer now owns
the animation frame-rate budget): indicator visible within the
activation-latency target (≈100–200 ms) after `State=recording` is published;
renderer animations (wave ribbon, appear/dismiss transitions) sustain ≈60 fps
on the GLArea frame clock without blocking the compositor; audio-level updates
at ~15–20 Hz feeding the 20–30 Hz envelope smoothing (attack 35 ms / release
280 ms — R17f) whose output (~3 strands, GPU-rasterized from packed uniforms,
no FFT) renders at display refresh; stale/quiet decay within the bounded
windows (~300 ms stale); state push → visual update < 50 ms; publisher
capture-path baselines unchanged.

**Constraints**: focus-safe (never take key focus — DOCK-typed window, empty
input region, dismiss-only exception; FR-001/FR-024/FR-025);
push-to-talk (no mapped window while idle); **privacy** — the dictation
interface and the renderer carry state + level only, never transcript text,
and nothing is persisted or logged by default (constitution V); the host
carries no dictation data at all (its only bus surface is the member-less
presence name); offline (no network); the publisher must not regress the
capture path; every deliverable must release all processes/windows/timers/
subscriptions on disable or exit and re-init cleanly across Shell restart /
relogin (FR-021).

**Scale/Scope**: one new Rust crate (`client/myna-hud`: ~8–12 modules — pure
ports, GL shader wrapper, window/pill UI, D-Bus consumer, input region,
accent/motion tracking, lab + simulator modes, po/); the extension bundle
rewritten thin (~4 modules + `metadata.json`); `myna-desktop` loses
`indicator/gtk.rs` + the `ui-gtk` feature and gains a small policy module;
snapcraft gains the `gnome` extension and the command; Workshop/Makefile/CI
updates; spec/docs updates. Deleted outright: the extension's drawing
modules, `dbus.js`, `gettext.js`, `stylesheet.css`, `dev-lab/`,
`dev-lab-gpu/`, their tests, and the Cairo/GLSL lockstep apparatus.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v1.3.0. **(2026-08-26 revision — re-tiered)** This feature now
spans **three tiers**:

- The **`myna-desktop` D-Bus publisher + launcher policy** and the
  **`myna-hud` renderer application** are *shipped Rust system components* —
  all principles apply in full (TDD, integration-readiness, watermarks,
  Workshop, privacy). The renderer relocation *shrinks* the constitutional
  carve-out: what was harness-tier GJS drawing code is now test-first Rust
  behind seams (pure model modules, fake-proxy D-Bus lifecycle, fake presence
  policy), with only composited visuals and live compositor behavior on the
  manual acceptance path.
- The **GJS extension host** is a thin in-compositor shim that *cannot* be
  Rust (platform constraint of GNOME Shell). It is treated as
  **evaluation-harness-tier scaffolding** analogous to the Python testbed
  carve-out (Technology & Environment Constraints): exempt from the
  Rust-language rule and the strict test-first TDD requirement, with its
  *logic* (placement math, binary resolution, respawn policy, adoption
  idempotence, presence lifecycle) factored into pure modules that DO get
  GJS contract tests, and only live compositor behavior (dock typing, focus
  safety, click-through, repositioning) deferred to the manual acceptance +
  headless-Shell harness. It still MUST honour the privacy and offline
  invariants (V) — trivially, since it carries no dictation data at all.
- **(superseded)** The prior `dev-lab` carve-out (non-shipped second toolkit)
  is obsolete: the labs are modes of the shipped binary and inherit its
  guarantees (R25).

| Principle | Gate | Status |
|---|---|---|
| I. Red-Green TDD (post-ratification) | Publisher: unchanged (fake-bus seam, contract tables as tests). Renderer: pure modules (state/envelope/model/shader-conformance/HUD logic), D-Bus consumer lifecycle (fake proxy), simulator mapping, input-region geometry, and launcher policy (fake presence) all land test-first; env-gated EGL render check for the shader on real drivers. Extension host: harness-tier — pure host logic gets GJS contract tests; compositor behavior via headless-Shell harness where available + manual acceptance. | PASS (publisher, renderer); EXEMPT (host, harness-tier — shrunken) |
| II. Integration-Test Readiness | Publisher: unchanged. Renderer: hermetic on fake proxy; `MYNA_DBUS_TESTS`/`MYNA_HUD_GL_TESTS`-gated suites runnable identically on VM and hardware; the headless-Shell harness exercises the host against a stub window. | PASS (by design) |
| III. Performance Watermarks | Publisher: unchanged. Renderer: activation→visible, level cadence, envelope constants, and GLArea frame budget declared as watermarks (Technical Context); EGL render check asserts the shader compiles/rasterizes on real drivers. Host: placement computation is O(1) algebra — no watermark needed. | PASS |
| IV. Workshop-Based Dev Environment | New deps land in the Workshop definitions in the introducing PR: GTK4/libadwaita dev headers + `glslang-tools` (renderer SDK), EGL/`libgl1-mesa-dev` for the gated render test, snapcraft `gnome` extension (packaging). The `myna-shell` workshop keeps gjs + gnome-shell (host suites). | GATED — tracked |
| V. Privacy-First, Offline-First | Unchanged: state + level only on the wire; renderer renders/logs/persists no content and captures no audio; the host carries no dictation data (member-less presence name only); no network anywhere. | PASS (by design) |

**Post-Phase-1 re-check**: see the end of this file — re-evaluated after the
design artifacts; no new violations introduced.

## Project Structure

### Documentation (this feature)

```text
specs/004-gnome-shell-indicator/
├── plan.md              # This file
├── research.md          # Phase 0 output (R21-R27: the 2026-08-26 revision)
├── data-model.md        # Phase 1 output (E6/E7 new; E4 rewritten)
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── dbus-interface.md #   org.myna.Dictation (unchanged wire) + org.myna.Shell presence
│   ├── publisher.md      #   DbusIndicator/DbusTrigger + launcher policy (Rust)
│   └── extension.md      #   the thin host's guarantees (GJS, rewritten 2026-08-26)
├── checklists/
│   └── requirements.md  # from /speckit-specify
└── tasks.md             # /speckit-tasks output (regenerated 2026-08-26)
```

### Source Code (repository root) — 2026-08-26 revision

```text
client/
├── myna-hud/                   # NEW CRATE — the renderer application (shipped Rust)
│   ├── Cargo.toml              #   gtk4 (v4_10), libadwaita (no version features —
│   │                           #     runtime property probing, R26), zbus, gl, gettext-rs
│   ├── src/
│   │   ├── main.rs             #   adw::Application; modes: hosted / --lab / --serve-dbus
│   │   ├── states.rs           #   wire state → descriptor (port of states.js; i18n: domain `myna`)
│   │   ├── vumeter.rs          #   calibrated envelope + stale-decay (port of vumeter.js)
│   │   ├── ribbon.rs           #   strand model + phase timing + envelope ballistics
│   │   │                       #     (port of ribbon.js incl. R17a/R17d/R17f)
│   │   ├── shader.rs           #   GLSL generator + tuning constants + uniform packing
│   │   │                       #     (port of ribbonGlsl.js + ribbonPaint.js tables;
│   │   │                       #     Cairo painter NOT ported — GPU-only, R23)
│   │   ├── hud_logic.rs        #   icon/phase/color-class/notice rules (port of hudLogic.js)
│   │   ├── accent.rs           #   libadwaita accent + user-value rule + palette derivation
│   │   │                       #     (R26; GSettings user-value guard)
│   │   ├── motion.rs           #   gtk-interface-reduced-motion + enable-animations fallback
│   │   │                       #     (absent-safe — never read the new a11y key directly)
│   │   ├── dbus_consumer.rs    #   org.myna.Dictation proxy: name watch + PropertiesChanged,
│   │   │                       #     injectable seam for hermetic tests (port of dbus.js)
│   │   ├── window.rs           #   the pill window: layout, input region per state (R22),
│   │   │                       #     appear/dismiss animation, a11y labels
│   │   ├── gl_area.rs          #   GLArea + gl wrapper: dual-profile compile, uniforms,
│   │   │                       #     frame-clock driving (port of ribbon_gl.py's wrapper)
│   │   ├── input_region.rs     #   pure: state → region rects (empty / dismiss rect)
│   │   ├── lab.rs              #   --lab controls (state/severity/level/motion + text view)
│   │   └── simulator.rs        #   --serve-dbus publisher (port of dictation_service.py)
│   ├── po/                     #   gettext domain `myna` (moved from extensions/myna-shell/po)
│   └── tests/
│       ├── states.rs           #   port of states.test.js
│       ├── vumeter.rs          #   port of vumeter.test.js
│       ├── ribbon.rs           #   port of ribbon.test.js (model/ballistics/phases)
│       ├── shader.rs           #   port of ribbonGlsl.test.js (#define conformance,
│       │                       #     uniform packing) + glslangValidator parse when available
│       ├── hud_logic.rs        #   port of hud.test.js
│       ├── dbus_consumer.rs    #   port of lifecycle.test.js (fake-proxy seam)
│       ├── input_region.rs     #   new: per-state regions
│       ├── simulator.rs        #   port of dbus_headless.py's mapping checks
│       └── render_gl.rs        #   env-gated (MYNA_HUD_GL_TESTS=1): surfaceless-EGL render
│                               #     check (port of render_headless.py)
├── myna-desktop/               # EXTENDED/REDUCED
│   └── src/
│       ├── indicator/
│       │   ├── gtk.rs          #   DELETED (ui-gtk removed; superseded by myna-hud)
│       │   └── ...
│       ├── policy.rs           #   NEW: presence watch → indicator-surface selection
│       │                       #     (P20-P23; fake presence seam for tests)
│       └── bin/myna-desktop.rs #   --overlay removed; policy wired
└── Cargo.toml                  # workspace members += "myna-hud"

extensions/
└── myna-shell/                 # REWRITTEN THIN — the overlay host
    ├── metadata.json           #   unchanged shell-version ["50","51"]
    ├── extension.js            #   enable/disable: presence name + spawn + adopt + supervise
    ├── host.js                 #   WaylandClient spawn, adoption, dock-typing, positioning,
    │                           #     anti-feedback reposition, respawn supervisor
    ├── place.js                #   PURE placement math (bottom-center of work area) —
    │                           #     the GJS-testable core (XH1)
    ├── presence.js             #   org.myna.Shell name ownership (XH5)
    ├── respawn.js              #   PURE respawn policy (XH3)
    ├── resolve.js              #   PURE binary resolution order (XH2)
    └── test/
        ├── place.test.js       #   placement math incl. monitor layouts
        ├── respawn.test.js     #   backoff/budget policy
        ├── resolve.test.js     #   resolution order + failure states
        ├── presence.test.js    #   name lifecycle (stub bus)
        └── host.test.js        #   adoption idempotence (fake window objects)

myna-snap/snap/snapcraft.yaml   # gnome extension; myna-hud app/command; wayland/x11 plugs back
.workshop/                      # SDK deps for the renderer (GTK4/libadwaita, glslang, EGL)
```

**Structure Decision**: The shipped, logic-bearing halves live in Rust in the
client workspace — the existing publisher plus the **new `myna-hud` crate**
(pure model/render modules test-first; UI shell; consumer; lab/simulator
modes). The extension bundle keeps its top-level `extensions/myna-shell/`
location (the GNOME loader demands the fixed layout) but shrinks to host
modules only; its logic is factored into pure GJS modules (`place.js`,
`respawn.js`, `resolve.js`, `presence.js`) so everything except live
compositor calls is headlessly testable. The member-less `org.myna.Shell`
presence name is the single seam between host and client policy; the
unchanged `org.myna.Dictation` contract is the seam between publisher and
renderer. Deleting the extension's drawing modules (and the two labs)
removes the entire Cairo/GLSL lockstep apparatus — `myna-hud`'s shader
generator and its conformance tests are the one source of truth (R23).

## Complexity Tracking

> Only rows that need constitutional justification. **(2026-08-26)** rows 1-2
> are superseded/shrunk by the architecture revision; new rows record the
> snap's GTK re-staging and the remaining thin-host carve-out.

| Violation / Risk | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| **GJS (non-Rust) extension host — thin shim** *(supersedes the 2026-07-21 row, shrunken 2026-08-26)* | The window-management host runs *inside Mutter* and MUST be GJS — there is no Rust option for a GNOME Shell extension. It is now a thin shim (launch/adopt/position/supervise/presence: ~4 modules, no drawing, no dictation data), not the renderer. | (a) Keeping the *renderer* in GJS too — rejected by the 2026-08-26 revision (R21): it forced the dual Cairo/GLSL rasterizers, the Shell-50 fallback tier, and the widest part of the constitutional carve-out; (b) a Rust `gtk4-layer-shell` overlay — still not implemented by Mutter, so it cannot host anything on GNOME; (c) dropping the extension entirely — an unassisted client window cannot be always-on-top, non-focus-stealing, click-through on GNOME (R1's survey finding still holds for unassisted clients). |
| **Host exempt from strict TDD + watermark baselines** *(shrunken 2026-08-26)* | Only *live compositor behavior* (dock typing, focus safety, click-through, repositioning) escapes unit tests — mirrors the Python-testbed carve-out for evaluation-harness scaffolding. All host *logic* (placement math, respawn policy, binary resolution, presence lifecycle, adoption idempotence) is pure GJS modules WITH contract tests; the renderer — now the logic-bearing, shipped UI — is fully TDD Rust. | Requiring test-first coverage of mutter interactions would test a mock of the Shell, not the integration where bugs live (constitution II rationale). The 2026-07-21 row's scope (whole extension incl. ribbon/accent/states) shrank to compositor calls only. |
| **New top-level `extensions/` tree outside the Cargo workspace** | The GJS bundle has no place in a Rust workspace and follows GNOME's fixed extension layout (`metadata.json` + ESM modules at the bundle root). | Nesting it under a crate would fight both `cargo` and the GNOME extension loader (which expects the bundle as-is under `~/.local/share/gnome-shell/extensions/<uuid>/`). A sibling top-level tree keeps each toolchain clean. |
| **New Workshop deps** *(amended 2026-08-26)*: GTK4/libadwaita dev headers + `glslang-tools` + EGL for the renderer's tests; snapcraft `gnome` extension for packaging | Constitution IV mandates the Workshop definition gain deps in the introducing PR. | Deferring violates IV; scoped as a foundational task extending the Workshop definitions. |
| **(2026-07-30, historical) `IndicatorState::Error` field addition ripples across 6 files** | The recoverable/critical severity distinction needed to reach `DbusIndicator::map_state` without fabricating a fake "error" transition for a successful, empty-transcript completion; shared helper across both call sites. | *(Historical row — unchanged by the revision; `gtk.rs` is since deleted, which removes one of the six sites.)* (a) A new top-level variant — same ripple, less coherent; (b) side-channel past the trait object — non-idiomatic; (c) a separate `ErrorSeverity` property + synthesized error — semantically wrong for a success path. |
| **(2026-08-26) GTK4 + libadwaita re-staged into the myna snap (~13 MB + icon themes)** — inverts the T69 demotion | The shipped indicator is now the GTK application itself; the toolkit cost buys the real product (GPU ribbon, accent color, lab/simulator modes), not a duplicate one-label window. | (a) Keep the snap slim and ship `myna-hud` as a deb/flatpak — fragments the well-known-`/snap/bin/myna-hud` path across formats and complicates the extension's resolution order; (b) render nothing when hosted — that is the status quo this change replaces. T69's audit remains correct *for what `ui-gtk` was*; recorded as inverted, not wrong. |
| **(2026-08-26) Renderer visuals verified by manual acceptance + env-gated EGL check, not pixel-unit tests** | The pill's composited look (GLArea output, animations, entrance feel) is a GPU/display property; the *decisions* are unit-tested (model, shader conformance, uniform packing), and the EGL check proves the shader rasterizes non-degenerate frames on real drivers without a display. | Pixel-diff golden tests would pin the renderer to incidental driver output and flake across Mesa/GLES versions; the old Cairo lockstep tests existed only because two rasterizers had to agree — with one rasterizer (R23) there is nothing to keep in lockstep. |

## Constitution re-check (post-design)

Re-evaluated after Phase 1 (research + data-model + contracts + quickstart),
**and again after the 2026-08-26 architecture revision**:

- **I. TDD** — Publisher: unchanged (row-per-guarantee tables as hermetic Rust
  tests first). **(2026-08-26)** The renderer's pure modules (states, vumeter,
  ribbon, shader generator conformance, HUD logic, input-region geometry), the
  D-Bus consumer lifecycle (fake-proxy seam), the simulator mapping, and the
  launcher policy (fake presence) all land test-first in `myna-hud`/
  `myna-desktop` — the previous GJS-tier ribbon/accent/states tests are
  *promoted* into the Rust tier by the port. The host's pure logic
  (place/respawn/resolve/presence/adoption) keeps GJS contract tests;
  live compositor behavior stays manual + headless-Shell harness (harness-tier,
  shrunken). The env-gated EGL render check proves the shader on real drivers.
  PASS (publisher, renderer) / EXEMPT (host, compositor behavior only).
- **II. Integration readiness** — publisher hermetic on a fake bus; renderer
  hermetic on a fake proxy; `MYNA_DBUS_TESTS`/`MYNA_HUD_GL_TESTS` suites
  runnable identically on VM and hardware; the headless-Shell harness drives
  the host against a stub window; the `--serve-dbus` simulator gives the
  acceptance a backend-free path (a deliberate 2026-08-26 requirement). PASS.
- **III. Watermarks** — publisher per-state overhead + level-pump cadence
  unchanged; the renderer owns activation→visible, level cadence, envelope
  constants, and the GLArea frame budget as declared watermarks; host placement
  is O(1) algebra. PASS.
- **IV. Workshop** — the open gate: GTK4/libadwaita dev headers +
  `glslang-tools` + EGL for the renderer's SDK, snapcraft `gnome` extension for
  packaging, in the introducing PR; the `myna-shell` workshop keeps gjs +
  gnome-shell for the host suites. GATED until those land.
- **V. Privacy/offline** — unchanged in substance: state + normalized level
  only on the dictation wire; nothing transcript-shaped is rendered, logged,
  or persisted; no network anywhere; capture path and buffers unchanged.
  **(2026-08-26)** The host now carries *no* dictation data at all (its only
  bus surface is the member-less presence name); the renderer's lab text area
  behaves like any ordinary text-editing app (same posture the old `dev-lab`
  had); the severity classification remains content-free. PASS.

No principle is violated by the revised design; the tracked items are the
shrunken GJS host tiering and its compositor-only TDD exemption (Complexity
Tracking, accepted), the Workshop deps (IV, foundational task), the historical
`IndicatorState::Error` ripple (2026-07-30, accepted), the snap's GTK
re-staging inversion of T69 (2026-08-26, accepted), and the visuals-verification
tier for the single remaining rasterizer (2026-08-26, accepted).
