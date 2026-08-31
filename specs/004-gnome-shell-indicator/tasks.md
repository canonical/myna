# Tasks: GNOME Shell Extension for Myna Dictation UI

**Input**: Design documents from `/specs/004-gnome-shell-indicator/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Regenerated 2026-08-26** for the architecture revision (spec Clarifications
2026-08-26; research R21–R27): the indicator is now drawn by the standalone
Rust GTK4 application `client/myna-hud` (GPU/GLSL-only), hosted as a focus-safe
overlay by a rewritten thin `extensions/myna-shell/` (launch/adopt/position/
supervise/presence — no drawing), with `myna-desktop` gaining the
presence-driven launcher policy and losing `ui-gtk`. Same audit discipline as
the 2026-07-30 regenerations: the ledger below records the disposition of every
prior task; only the revision's delta is new work.

**Tests**: Three tiers (plan Constitution Check).
- The **Rust publisher + policy** in `myna-desktop` and the **`myna-hud`
  renderer** are *shipped system components*: constitution Principle I
  (Red-Green TDD) applies in full — behavior-bearing tasks are preceded by
  failing hermetic tests over fake seams (fake `Bus`, fake presence, fake
  D-Bus proxy), and real-bus/GL behavior is proven by env-gated suites
  (`MYNA_DBUS_TESTS=1`, `MYNA_HUD_GL_TESTS=1`) runnable identically on the
  desktop VM and on hardware (Principle II).
- The **GJS extension host** is *evaluation-harness-tier* (plan Complexity
  Tracking): its pure logic (placement math, binary resolution, respawn
  policy, presence lifecycle, adoption idempotence) gets GJS contract tests;
  live compositor behavior (dock typing, focus safety, click-through,
  repositioning) is proven by the headless-Shell harness where available plus
  the manual on-hardware acceptance (quickstart §5/§5a/§5b).
- The **lab/simulator modes** are capabilities of the shipped binary: the
  simulator's state mapping is unit-tested; the lab UI itself carries no
  acceptance weight (it drives the identical renderer modules).

**Organization**: phases ordered by dependency: Foundational (crate + deps +
spikes) → Pure-logic port → Renderer app → Lab/simulator → Extension host →
Client policy/removal → Packaging → Deletions → Docs → Acceptance. Story
labels (US1/US2/US2A/US3/US4) annotate where the user-visible guarantees land;
the renderer phases realize US1/US2/US2A/US3 guarantees in one place (the
app), the host phases realize US1's focus-safety/positioning (FR-024–FR-027).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel (different files, no dependency on incomplete tasks)
- **[Story]**: US1/US2/US2A/US3/US4 where a user-visible guarantee lands; infra phases carry none
- All paths are repo-relative

## Path Conventions

- Renderer crate (new, shipped): `client/myna-hud/` (`src/{main,states,vumeter,ribbon,shader,hud_logic,accent,motion,dbus_consumer,window,gl_area,input_region,lab,simulator}.rs`, `po/`, `tests/`)
- Client policy: `client/myna-desktop/src/policy.rs` (+ tests); removal: `client/myna-desktop/src/indicator/gtk.rs`, `Cargo.toml` `ui-gtk`
- Extension host (rewritten): `extensions/myna-shell/` (`extension.js`, `host.js`, `place.js`, `respawn.js`, `resolve.js`, `presence.js`, `test/`)
- Publisher (existing, unchanged): `client/myna-desktop/src/{dbus/,indicator/dbus.rs,shortcut/dbus.rs}`
- Shared contracts: `specs/004-gnome-shell-indicator/contracts/{dbus-interface,publisher,extension}.md`
- Packaging: `myna-snap/snap/snapcraft.yaml`; envs: `.workshop/myna.yaml`, `.workshop/myna-shell.yaml`, `Makefile`, `.github/workflows/ci.yml`

## Prior-task ledger (audit 2026-08-26)

| Prior tasks | Disposition |
|---|---|
| T001–T008 (setup, Bus seam, base mappings) | **Complete, kept** — publisher infra unaffected by the revision |
| T009–T017 (severity split) | **Complete, kept** — wire contract unchanged; `gtk.rs` site deleted by T150 |
| T018–T020 (publisher activation, dbus.js proxy) | Publisher half **kept**; `dbus.js` **deleted** with the old bundle (its port is T111/T124) |
| T021–T025, T026–T028, T029–T035 (HUD actor, state/severity treatments in GJS) | **Superseded** — logic ports to `myna-hud` (T102–T109 pure ports, T112–T114 app wiring); the St/Clutter actors are deleted (T170) |
| T036–T039a (level pump, calibrated envelope) | Level pump **kept**; envelope math **ported** (T102) |
| T051–T058a, T056a–T056k (wave ribbon + refinements) | **Superseded** — pure logic ports 1:1 (T102–T104); Cairo painter dropped (GPU-only, R23); the GLSL generator ports with its conformance tests (T104) |
| T059–T064 (dev-lab) | **Superseded** — replaced by the app's `--lab` mode (T131–T133); files deleted (T170) |
| T040–T043 (US4 panel toggle / DbusTrigger) | **Still open, unchanged** — carried forward as T140–T141 (panel button now belongs to the host/extension side; `DbusTrigger` unchanged) |
| T044–T046 (gated round-trip, watermarks) | T044 **kept**; T045 **extended** (C12/C13 — T151); T046 **kept** + renderer watermarks (T152) |
| T047–T048 (high-contrast, version gate) | **Carried forward** as T153–T154 (now app-side CSS / host-side gate) |
| T049–T050a (docs, acceptance) | T049 **superseded** by T171 (docs rewrite for the revision); T050/T050a **carried forward** as T180–T181 |

---

## Phase A: Foundational — crate, deps, spikes (Setup)

**Audit notes**: nothing here exists yet. The spikes de-risk the two
protocol assumptions in R21/R27 before any product code lands.

- [X] T100 [P] Workspace + crate scaffold: add `myna-hud` to `client/Cargo.toml` members; `client/myna-hud/Cargo.toml` with `gtk4` (`v4_10`), `libadwaita` (no version-gated features — runtime property probing for the 1.7/4.22 surfaces, R26), `zbus`, `gl`, `gettext-rs` (`gettext-system`); an empty `main.rs` (adw::Application shell). Workshop SDK deps land in the same PR (constitution IV): GTK4/libadwaita dev headers, `glslang-tools`, `libegl-dev`/`libgl1-mesa-dev` for the gated render test (`.workshop/myna.yaml` desktop SDK). *(Done 2026-08-26: crate builds against the container's GTK 4.22/adw 1.9; the runtime-version-matrix decision — snap gnome-46-2404 SDK GTK 4.18.6/adw 1.7.7 with Yaru patches, workshop 24.04, hosts 26.04 — is recorded in plan.md Primary Dependencies and R26; gettext uses the system provider since the vendored gnu gettext does not build in minimal containers.)*
- [X] T101 [P] **Spike: GLArea GPU path** — a throwaway example (not shipped) that opens a `Gtk.GLArea` under `xvfb`, compiles a trivial ES-300 fragment shader via the `gl` crate (`load_with` on GDK's `get_proc_address`), and draws a gradient; run under `xvfb-run` in the container. Output: the dual-profile wrapper design (gl120/es300) confirmed, recorded in `gl_area.rs`'s module docs. *(De-risks R23; the wrapper itself is T121.)* *(Done 2026-08-26: `examples/gl_spike.rs`, green under `xvfb-run`. Mesa reports **OpenGL ES 3.2** — GTK4 takes a GLES context even on X11, confirming ES 3.00 as the production profile. Discovery: Ubuntu's libepoxy exports no generic `epoxy_get_proc_address`, only per-function dispatch pointers, so the renderer declares those `epoxy_gl*` extern statics instead of a `load_with` loader; the `gl` crate supplies types/constants. The REAL generated shader compiled on the driver.)*
- [ ] T101a [P] **Spike: WaylandClient host** — extend the existing headless-Shell harness (`extensions/myna-shell/test/entrance-visual.sh`'s pattern) to load a driver that calls `Meta.WaylandClient.new_subprocess` on a gjs stub "HUD" window, adopts it via `owns_window`, `set_type(DOCK)`, `hide_from_window_list`, `stick`, `make_above`, `move_frame`, and asserts the window's type/position. *(De-risks R21 on both target mutter ABI generations — the workshop carries Shell 50; `next-shell.sh` covers 51.)*
- [ ] T101b [P] **Validate host adoption against a real snapped GTK app** (rescoped 2026-08-26) — the `WAYLAND_SOCKET` fd-handoff worry is treated as **low-risk**: a confined GTK/snap app normally receives the compositor socket through the wrapper (that is how every snapped GTK app connects), so `Meta.WaylandClient.new_subprocess` inheriting it is the expected case, not the exception. Rather than a bespoke minimal-snap spike, validate the *whole* adoption path (spawn → `owns_window` → DOCK/hide/above/position) by hosting an existing **snap-packaged GTK app** available in the maintainer's test environment as a stand-in HUD, and confirm the socket reaches it and the window is adopted. If (and only if) `owns_window` fails there, fall back to `get_sandboxed_app_id()`/PID matching (R27). T123 uses whichever path this confirms.

**Checkpoint**: `cargo build -p myna-hud` works; all three spikes recorded.

## Phase B: Pure-logic port (TDD — every task red-first)

**Audit notes**: the GJS sources are the specification; ports are 1:1 unless a
decision says otherwise (R23: no Cairo painter; R26: accent sourcing).

- [X] T102 [US3] Port `vumeter.js` → `client/myna-hud/src/vumeter.rs` + `tests/vumeter.rs` (port of `test/vumeter.test.js`): dBFS calibration (`DB_FLOOR=-67`, `DB_CEILING=-14`), `PEAK_WEIGHT=0.55` blend, arrival-time stale-decay (`STALE_MS=300`), floor. **Test first.** *(Done: 9 ported assertions.)*
- [X] T103 [US3] Port `ribbon.js` → `ribbon.rs` + `tests/ribbon.rs` (port of `test/ribbon.test.js`): strand model (`base`/`voice`/`secondary`), phase machine (unfold 175 ms / flow / morph 225 ms / complete 400 ms; `relax` stays removed), `applyEnvelopeSmoothing` attack/release ballistics (35/280 ms), crest-brightness, strong-syllable detection (detection-only), severity tint, `elapsedMs` echo. **Test first.** *(Done: 16 ported assertions.)*
- [X] T104 [US3] Port `ribbonGlsl.js` + the tuning tables of `ribbonPaint.js` → `shader.rs` + `tests/shader.rs` (port of `test/ribbonGlsl.test.js`): shader generator with `#define`s baked from the Rust constants (every define asserted equal to its constant), gradient `mix()` chains, Gaussian stacks, uniform list + `pack_ribbon_uniforms` (vec2–4 packing). Add a `glslangValidator` parse check when the binary is present (skip cleanly otherwise). **The Cairo painter is NOT ported** (R23). **Test first.** *(Done: 11 assertions incl. real glslangValidator compilation in GL 1.20 / ES 1.00 / ES 3.00, and the strandY sine-mirror check at <1e-12.)*
- [X] T105 [US2] Port `states.js` → `states.rs` + `tests/states.rs` (port of `test/states.test.js`): descriptor `{key, statusText, severity, hidden}`, unknown→ACTIVE tolerance, `notice`/`error` severity split, content-free reason formatting. i18n via `gettext-rs` domain **`myna`** (R25); tests assert the English source like the GJS suite did. **Test first.** *(Done: 8 ported assertions.)*
- [X] T106 [US2/US2A] Port `hudLogic.js` → `hud_logic.rs` + `tests/hud_logic.rs` (port of `test/hud.test.js`): `icon_for_severity`, `severity_auto_dismisses`, `should_replace_held_notice`, `pill_color_class` (+ class list), `ribbon_phase_for_state_key`, `ribbon_visible_for_severity`. **Test first.** *(Done: 6 ported assertions.)*
- [X] T107 [P] New `input_region.rs` + `tests/input_region.rs`: pure state → input-region rects (empty everywhere; the dismiss control's rect during critical error; recompute on size change — R22). **Test first.** *(Done: 5 assertions.)*
- [X] T108 [P] New `accent.rs` + `tests/accent.rs` (port of the *rules* in `test/accent.test.js`): GSettings user-value guard for `org.gnome.desktop.interface` `accent-color` (`null`/schema-absent → Ubuntu-orange `#E95420`); chosen → libadwaita `AdwStyleManager` accent as main color; derived palette (highlight / darker-complement / translucent; **aubergine instead of complement when orange**) as pure, tested logic (R26). **Test first.** *(Done: 9 assertions incl. the new R26 platform-accent path.)*
- [X] T109 [P] New `motion.rs` + `tests/motion.rs`: reduced-motion resolution — `gtk-interface-reduced-motion` via GtkSettings (GTK ≥ 4.22), fallback to inverted `enable-animations` (schema-guarded), **never** a direct read of the new `org.gnome.desktop.a11y.interface reduced-motion` key (E2b's crash guard; unit-test the fallback selection with a fake settings seam). **Test first.** *(Done: 4 assertions; the crash guard is structural — MotionReadings carries only the two safe sources.)*
- [X] T110 [P] Simulator mapping: `simulator.rs` mapping table + `tests/simulator.rs` — the lab-controls → wire-state inverse mapping and `envelope_to_levels` (port of `dictation_service.py`'s logic incl. its deliberate `boostLevel` transcription so drift stays detectable). **Test first.** *(Done: 8 assertions incl. the exact envelope round-trip through the real vumeter.)*

**Checkpoint**: `cargo test -p myna-hud` green — 76 assertions across 9
suites (vumeter, states, ribbon, shader, hud_logic, input_region, motion,
accent, simulator); the ported suites match their GJS counterparts
assertion-for-assertion, and the generated shader compiles on a real
driver. **Phase B complete (2026-08-26).**

## Phase C: The renderer application (US1/US2/US2A/US3)

- [X] T111 [US1] D-Bus consumer `dbus_consumer.rs` + `tests/dbus_consumer.rs` (port of `dbus.js` semantics + `test/lifecycle.test.js`): name-watch (dormant/appeared/vanished), async proxy creation, `PropertiesChanged` subscription, cached-snapshot reflection, no level dedup (arrival-time freshness — R16a bug 1), injectable proxy/watch seams for hermetic tests. **Test first.** *(Done 2026-08-26: 9 tests; the two dedup rules pinned — levels never, state always.)*
- [X] T112 [P] App shell `window.rs`: the pill window — borderless, no decorations, sized/shaped for the bottom-center pill; per-state mapping (hidden while idle, FR-002); entrance/dismiss transitions within the latency targets (FR-003); a11y labels per state; appears/dismisses driven by `states.rs` + `hud_logic.rs`. *(Done 2026-08-26: plain GtkApplicationWindow — adw::ApplicationWindow imposes a 200px height floor, measured; all six states captured under xvfb.)*
- [X] T113 [US2/US2A] Held-notice slot in the app (R15): replace-in-place, restart-timer, no stacking — port of T031/T034's semantics now in Rust (test-first on the pure slot type, then wire into `window.rs`). *(Done 2026-08-26: `notice_slot.rs`, 7 tests; pure and clock-free.)*
- [X] T114 [US2A] Dismiss (×) control: rendered during critical error only; click clears the held notice; the window's input region covers exactly its rect (T107) — re-applied on map and size-allocate; never keyboard-focusable (no focusable widgets; window not focusable on map). *(Done 2026-08-26: empty input region; the dismiss rect punched back for a critical error only.)*
- [X] T121 [US3] `gl_area.rs` (from T101's spike, now product code): GLArea + `gl` wrapper with dual-profile compile (gl120/es300), per-frame uniform upload from `ribbon.rs`/`shader.rs`, frame-clock driving gated on mapped + not reduced-motion; wire into `window.rs`. *(Done 2026-08-26: epoxy extern statics; ES 3.00 on Mesa; render_check reads pixels back and pins the UV feed.)*
- [X] T122 [US3] Accent/motion wiring: live re-resolution (settings changed signals → palette/motion update without restart); reduced-motion static alternative rendering path. *(Done 2026-08-26: runtime property probing; gtk-interface-reduced-motion is an ENUM — fixed and pinned.)*
- [X] T124 `main.rs` modes: default (hosted/standalone — consume `com.canonical.Myna.Dictation`); parse `--lab`/`--serve-dbus`. Run with GTK on the main thread; the consumer on a worker; channel bridge (the inverse of the old `GtkIndicator` pattern). *(Done 2026-08-26: argv-parsed modes; zbus worker + async-channel bridge.)*

**Checkpoint**: `cargo run -p myna-hud` (with a stub publisher or the simulator) shows the full pill — states, severities, ribbon — in a plain window.

## Phase D: Lab & simulator modes (R25)

- [X] T131 [P] `--lab` UI (`lab.rs`): manual controls (state, severity, level slider, reduced-motion toggle, phase triggers) + a plain `Gtk.TextView` dictation target, driving the identical renderer modules with **no backend** (T124's mode dispatch). *(Done 2026-08-26: state/level/session controls + dictation target; publishes at the contract cadence.)*
- [X] T132 `--serve-dbus` (`simulator.rs` serving): claim `com.canonical.Myna.Dictation` (never by force; clean release), publish State/ErrorMessage/AudioRms/AudioPeak at ~20 Hz from the lab controls, implement Start/Stop/Toggle (port of `dictation_service.py` + `dbus_headless.py`'s contract checks as a hermetic test over the fake `Bus`-style seam). *(Done 2026-08-26: `session_control.rs` (7 tests, C6 dedup) + `serve.rs` (zbus server, ~20Hz publish, Start/Stop/Toggle); `tests/serve_roundtrip.rs` claims the real name and round-trips over `dbus-run-session` — State/levels/methods/stand-down; `--serve-dbus` wired via `lab::present_serving`.)*
- [X] T133 [P] i18n move: `client/myna-hud/po/` (domain `myna`; `POTFILES.in` from Rust sources via xgettext), replacing `extensions/myna-shell/po/`. *(Done 2026-08-27: `client/myna-hud/po/` — POTFILES.in, regenerated myna.pot (18 msgids: lab UI + status strings), LINGUAS, README; status msgids marked `n_()` (port of N_, extractable while looked up by variable); domain `myna` bound in main.rs via gettextrs TextDomain with MYNA_HUD_LOCALEDIR override; `make i18n` regenerates the template. Old extension po/ removed in T170.)*

**Checkpoint**: quickstart §3a/§3b usable — the lab renders with no backend; the simulator drives a hosted indicator end-to-end.

## Phase E: The extension host rewrite (FR-024–FR-027, XH1–XH13)

- [X] T120 [US1] Pure host modules, **test first** (GJS, port of the discipline not the code): `place.js` + `test/place.test.js` (bottom-center math, monitor layouts, XH1); `resolve.js` + `test/resolve.test.js` (`$MYNA_HUD_BINARY` → `/snap/bin/myna-hud` → `/usr/bin/myna-hud`, failure states, XH2); `respawn.js` + `test/respawn.test.js` (bounded backoff + restart budget → dormancy, XH3); `presence.js` + `test/presence.test.js` (`com.canonical.Myna.Shell` own/release lifecycle, fail-soft, XH5); `host.test.js` adoption idempotence with fake window objects (XH4). *(Done 2026-08-26: place/resolve/respawn/presence + 50 assertions across four gjs suites, green under gjs 1.88.)*
- [X] T123 [US1] `host.js` + `extension.js`: `Meta.WaylandClient.new_subprocess` spawn (T101b's chosen path), `window-created` adoption via `owns_window` (fallback: `get_sandboxed_app_id`/PID), `set_type(DOCK)` + `hide_from_window_list` + `stick` + `make_above`, `move_frame` positioning with anti-feedback guards, reposition on monitors/workarea/size changes, exit watch → respawn policy, `disable()` → terminate + release name + disconnect all. *(Done 2026-08-26: `host.js` — Meta.WaylandClient.new_subprocess launch, window-created adoption via owns_window, DOCK-type + hide_from_window_list + stick + make_above, move_frame placement with anti-feedback muting, workareas/monitors/size reposition, subprocess exit-watch → planRestart policy, disable → destroy + disconnect all; `extension.js` rewired to host + presence, no longer draws or consumes state; `test/host.test.js` composed-logic smoke; APIs verified in Meta-18.gir and Meta-51.gir. Launch is `snap run myna.hud` per maintainer direction.)*
- [ ] T125 [P] Headless-Shell integration test (extends T101a's driver into a committed test): spawn a stub HUD window, assert adoption/typing/positioning end-to-end in the harness; skip cleanly (exit 77) where the environment can't run it.

**Checkpoint**: with the extension enabled and `MYNA_HUD_BINARY` set, the pill appears hosted: no window-list entry, all workspaces, always-on-top, click-through, correctly positioned.

## Phase F: Client policy & removal (P20–P23)

- [X] T150 Remove `ui-gtk`: delete `client/myna-desktop/src/indicator/gtk.rs`, the `ui-gtk` feature, `--overlay` CLI mode, and `async-channel`/`gtk4`/`glib` optional deps; update the affected tests (the P19 "unchanged behavior" assertions move to `notify.rs`-only). *(Done 2026-08-27: deleted `src/indicator/gtk.rs`, the `ui-gtk` feature and the `gtk4`/`glib`/`async-channel` optional deps; removed the `--overlay` CLI mode and `run_with_overlay`; updated `indicator/mod.rs`, `lib.rs`, `notify.rs`, `mock.rs` docs, `tests/indicator_hw.rs` (now just the clean skip gate), and the README/snap/docs references. myna-desktop lib tests (49) + indicator_hw pass.)*
- [X] T151 New `policy.rs` + `tests/policy.rs` (fake presence seam): suppress/restore the `NotifyIndicator` fallback on `com.canonical.Myna.Shell` appeared/vanished (P20/P21), bus errors degrade to fallback — never abort (P22), contract-only non-GNOME spawn seam (P23). **Test first.** Extend `tests/dbus_hw.rs` (env-gated) with the C12/C13 round-trips. *(Done 2026-08-27: `src/policy.rs` + `tests/policy.rs` — `Policy` trait + `SurfaceDecision::for_shell_presence` (P20/P21), bus-error→fallback (P22), contract-only spawn seam (P23); `probe_shell_presence()` queries the session bus via the crate's stale-guid-tolerant connect; `indicator::SilentIndicator` shipped no-op; the binary's bus-error fallback suppresses NotifyIndicator when com.canonical.Myna.Shell is present; `tests/dbus_hw.rs` gains the env-gated C12/C13 presence round-trip (green under dbus-run-session).)*

**Checkpoint**: `cargo test -p myna-desktop` green with the policy in and the overlay out; `--dbus` behavior unchanged.

## Phase G: Packaging, envs, CI

- [X] T152 [P] Renderer watermarks `client/myna-hud/tests/watermarks.rs`: activation→visible, envelope constants, GLArea frame budget (declared tolerances); publisher watermarks unchanged (T046 carried). *(Done 2026-08-27: `tests/watermarks.rs` — 7 watermark tests pinning the declared constants: activation→visible immediate (recording descriptor not hidden), envelope attack 35ms/release 280ms, stale-decay 300ms, publish cadence 15-20Hz, lifecycle durations in band, and the 60fps frame-budget (bounded per-frame envelope advance + convergence within ~200ms, stale decay at the boundary).)*
- [X] T160 Snap: `myna-snap/snap/snapcraft.yaml` gains the `gnome` extension, the `myna-hud` app/command, and the `wayland`/`x11` plugs back (R27); `make snap-myna` builds; smoke: `snap run myna.hud --help` inside the snap. *(Done 2026-08-27: `myna-hud` app added with `extensions: [gnome]` (GNOME 46 SDK) and `wayland`/`x11`/`desktop` plugs; `client` part gains `libgtk-4-dev`/`libadwaita-1-dev`/`libepoxy-dev`/`libegl-dev` build-packages, `cargo install --path myna-hud`, and the ELF-closure sweep now keeps `bin/myna-hud`; fixes T69's inversion — ~13 MB re-staged, accepted.)*
- [X] T161 [P] Envs/CI: `.workshop/myna.yaml` (renderer SDK deps from T100; `test`/`lint` now cover `myna-hud`), `.workshop/myna-shell.yaml` (host suites replace the drawing suites in `gjs-test`), `Makefile` doc-lines, `.github/workflows/ci.yml` (no new jobs — existing `workshop`/`extension` jobs pick up the crates/suites); `cargo deny`/`machete` clean for the new crate. *(Done 2026-08-30: verified rather than re-landed — `myna-hud` is a `client/Cargo.toml` workspace member, so `cargo test --workspace`/`cargo clippy --workspace` (the `test`/`lint` actions) already cover it with no job changes needed; `.workshop/myna.yaml`/`.workshop/myna-shell.yaml` SDK-dep and gjs-test updates were already in place; `Makefile` has no stale doc-lines (`client` builds the whole workspace, `i18n`/`install-schema`/`test-extension*` already reference the new host/renderer). `cargo machete` (installed 0.9.1, pinned to match the coverage SDK) reports no unused deps; `cargo deny check bans licenses` passes clean across the workspace incl. `myna-hud`.)*

## Phase H: Deletions (spec Assumptions: outright, no flags)

- [X] T170 Delete the old renderer surface: `extensions/myna-shell/{hud.js,hudLogic.js,view.js,states.js,vumeter.js,ribbon.js,ribbonPaint.js,ribbonGlsl.js,ribbonShader.js,accent.js,dbus.js,gettext.js,stylesheet.css}`, `test/{states,hud,lifecycle,vumeter,ribbon,accent,ribbonGlsl}.test.js`, `test/gpu-probe.{js,sh}`, `dev-lab/`, `dev-lab-gpu/`, `po/`; rewrite `extensions/myna-shell/README.md` as the host's README; drop the Cairo/GLSL lockstep apparatus from CI paths. *(Only after Phase E is green — the host replaces the drawing.)* *(Done 2026-08-27: deleted the entire old renderer surface — the 13 drawing/consumer modules, the 7 drawing test suites, gpu-probe.{js,sh}, entrance-visual.sh + visual-driver/, dev-lab/ and dev-lab-gpu/, and the old po/. run-suite.sh now runs only the host suites (5 test files, 68 assertions, green); README rewritten as the host README; next-shell.sh / CI / .workshop/myna-shell.yaml updated to drop the Cairo/GLSL lockstep apparatus.)*
- [X] T171 [P] Docs: `docs/desktop-injection.md` §2 + Future — qualify "no sanctioned way" for *unassisted* clients; record the extension-hosted overlay as the GNOME answer and the layer-shell backend as a backend swap; `docs/project-plan.md` T69 inversion note; root `README.md` indicator paragraphs; `client/README.md` crate list. *(Done 2026-08-27: `desktop-injection.md` §2 now says "no sanctioned way for an *unassisted* normal client" and notes the assisted `Meta.WaylandClient`-hosted path *is* sanctioned — the shipped GNOME answer (`myna-shell` + `myna-hud`); Future records the hosted overlay as the GNOME answer and `gtk4-layer-shell` as a backend swap behind the `Indicator` seam; `project-plan.md` T69 gains the `myna-hud` re-stage inversion note (T69's slim-snap gate still holds for `myna`); root `README` What’s-in-here + The-GNOME-Shell-indicator paragraphs updated to the hosted overlay; `client/README` crate list adds `myna-hud`.)*

## Phase I: US4 carried forward (P3, unchanged scope)

- [X] T140 Hermetic `DbusTrigger` tests + implementation (`client/myna-desktop/src/shortcut/dbus.rs`, C6/C7, P9–P12) — as originally scoped (T040/T042). *(Done 2026-08-27: `src/shortcut/dbus.rs` — `DbusTrigger` (Trigger) + `DbusTriggerSource` with Press/Release alternation and duplicate suppression (P9/P10), content-free `(false, reason)` refused-start shape (P11), clean exhaustion (P12); served object gains `Start`/`Stop`/`Toggle` feeding the source via `serve_with_trigger` (C6); 6 hermetic tests + env-gated wire round-trip in dbus_hw.rs.)*
- [ ] T141 Optional panel button (host side, `extension.js`/`presence.js`-adjacent) with availability dimming — T041/T043 re-homed; the button consumes `com.canonical.Myna.Dictation` directly (it is a client like any other).

## Phase J: Acceptance & gates

- [X] T153 [P] High-contrast legibility (app CSS): contrast variant; severity never color-only (T047 re-homed). *(Done 2026-08-27: `probe_high_contrast()` reads `GtkSettings:gtk-interface-contrast` (a `GtkInterfaceContrast` enum: unsupported=0/no_preference=1/more=2, read via g_value_get_enum like reduced-motion), wired into `watch_preferences` (notify::gtk-interface-contrast) and applied as the `.myna-hud-high-contrast` CSS class on the pill. Severity is already never colour-only: critical swaps to the mic-disabled icon + distinct label, recoverable has its own label text (existing x19 test).)*
- [ ] T154 [P] Version-gate verification: `metadata.json` `shell-version: ["50","51"]` loads on both (workshop + `next-shell.sh`); mutter ABI 18/51 host APIs asserted by T101a/T125 (T048 re-homed).
- [ ] T180 Run quickstart end-to-end (§1–§8): hermetic + gated suites, host contract tests, lab/simulator, install, the **on-hardware hosted spoken run** (focus never stolen — including the × click; clicks pass through; no window-list entry; all workspaces; always-on-top; not movable/minimizable/closable via ordinary window management; states legible; ribbon correct incl. accent/fallback and reduced-motion no-crash; transcript injected unchanged), severity walkthroughs, renderer-crash respawn + budgeted dormancy, disable teardown, watermarks (SC-001–SC-016).
- [ ] T181 [P] SC-013 structured comparison (carried from T050a): GPU ribbon vs. a recording of the prior implementation, ≥3 observers, majority-verdict recorded.

---

## Dependencies & Execution Order

### Phase Dependencies

- **A (Foundational)**: T100 ∥ T101 ∥ T101a ∥ T101b — all independent.
- **B (Pure port)**: after T100; internally T102 → T103 (envelope feeds the model) → T104 (shader consumes the model's uniforms); T105–T110 all [P] once T100 lands.
- **C (Renderer app)**: T111–T114 after T105/T106 (states/hud logic); T121 after T103/T104 + T101; T122 after T108/T109; T124 last (mode dispatch).
- **D (Lab/simulator)**: T131 after T124; T132 after T110/T111 (shares the consumer contract); T133 [P] anytime after T105.
- **E (Host)**: T120 [P] with Phase B/C (pure GJS, no deps on the app); T123 after T101a + T120 + a runnable `myna-hud` binary (Phase C checkpoint); T125 after T123.
- **F (Client)**: T150 [P] anytime; T151 after T111's seam shape (shares fake-bus idioms).
- **G (Packaging)**: T152 after T121; T160 after Phase C checkpoint (needs the binary); T161 [P] after T100/T120.
- **H (Deletions)**: T170 strictly after Phase E checkpoint; T171 [P] after the contracts are final (done).
- **I (US4)**: independent; T141 after T123 (panel presence lives in the host bundle).
- **J (Acceptance)**: T180 after everything; T153/T154/T181 [P] against their respective phases.

### MVP First

A → B → C (+ T123 from E) is the shippable core: a hosted, focus-safe,
state-legible pill with the ribbon. D (lab/simulator) lands alongside C for
iteration; F/G/H complete the delivery; I (US4) and J's structured comparison
are last.

### Parallel Opportunities

- T101 ∥ T101a ∥ T101b ∥ T100 (four independent tracks).
- Phase B's T105–T110 are mutually parallel after T100.
- Phase E's T120 is parallel with all of Phase C (different tree).
- T150 ∥ Phase C/D (different crate).
- T171 ∥ everything after the contracts (this document's siblings, already amended).

---

## Branch Staging Plan (REQUIRED — constitution "Staged Delivery in Feature Branches")

| # | Branch | Scope (phases/tasks) | Prerequisite branches | Merge gates |
|---|--------|----------------------|-----------------------|-------------|
| 1 | `004q-renderer-foundation` | A + B (T100–T110) | — | `cargo test -p myna-hud` green (ported suites); spikes recorded; Workshop deps landed |
| 2 | `004r-renderer-app` | C + D (T111–T114, T121–T124, T131–T133) | #1 | app runs standalone against the simulator: full pill incl. severities + ribbon; EGL render check green; lab usable |
| 3 | `004s-host` | E (T120, T123, T125) + T170 (deletions) | #2 (needs the binary) | host contract tests green; headless-Shell integration green (or skip-77); old renderer files deleted; `make test-extension[-next]` green |
| 4 | `004t-client-policy` | F (T150–T151) | — (parallel with #2/#3) | policy tests green; `ui-gtk` gone; workspace + clippy green |
| 5 | `004u-packaging` | G (T152, T160, T161) | #2, #4 | snap builds and smoke-runs `myna-hud`; CI/workshop green |
| 6 | `004v-docs-us4-acceptance` | I + J + T171 (T140–T141, T153–T154, T180–T181) | #1–#5 | quickstart §1–§8 pass on hardware; docs updated |

---

## Notes

- [P] = different files, no dependency on incomplete tasks.
- The Rust renderer and publisher/policy are TDD-first (Principle I); the GJS host is harness-tier — pure logic contract-tested, compositor behavior manual + headless-Shell. The lab UI drives identical modules and carries no acceptance weight.
- **Privacy invariant throughout**: only state (incl. the `notice`/`error` severity split) + normalized level + a content-free reason cross `com.canonical.Myna.Dictation`; the renderer renders/logs/persists no transcript; the **host carries no dictation data at all** (member-less presence name only); no audio captured; no network (constitution V). The ribbon stays a single smoothed envelope — never raw samples (R17).
- **Crash-on-start guards**: E2b — never construct/read GSettings for `org.gnome.desktop.a11y.interface reduced-motion` unguarded (new key, absent on older systems); accent-color reads keep the schema/key guard (R18/R26). T109's fake-seam tests pin this.
- `CARGO_HOME` note (container/dev): use a temporary `CARGO_HOME` (e.g. `/tmp/opencode/cargo-home`) for local builds so crates aren't kept across container restarts; CI/Workshop own the canonical environments.
- The host's permitted API surface is the public extension API + mutter's introspected WaylandClient/Window APIs (verified in `Meta-18.gir`/`Meta-51.gir`); no private Shell internals (`OsdWindow`, `Main.wm._checkDimming`, etc. — contract extension.md Constraints).
- Verify each test fails before implementing; commit after each task or logical group; stop at any checkpoint to validate independently.
