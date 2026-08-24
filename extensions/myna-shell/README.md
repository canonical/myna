# myna-shell — GNOME Shell dictation HUD

The focus-safe dictation indicator for GNOME (feature 004). A bottom-center
HUD pill, styled after GNOME's own volume/brightness OSD, that visualizes
`myna-desktop`'s dictation state and audio level. Pure UI: it never captures
audio, transcribes, or injects text — see `docs/desktop-injection.md` for the
last-mile that does. Contract and design history: `specs/004-gnome-shell-indicator/`.

## Install (development)

```sh
UUID=myna-shell@canonical.com
mkdir -p ~/.local/share/gnome-shell/extensions/$UUID
cp -r extensions/myna-shell/* ~/.local/share/gnome-shell/extensions/$UUID/
# dev-lab/ is a non-shipped development tool — never installed.
rm -rf ~/.local/share/gnome-shell/extensions/$UUID/dev-lab
gnome-extensions enable $UUID
```

GNOME Shell on Wayland does not hot-reload extension JS — after copying an
update, `gnome-extensions disable "$UUID" && gnome-extensions enable "$UUID"`
only refreshes `metadata.json`; a **log out / log back in** is required to
load changed module code. See `dev-lab/README.md` for a much faster
standalone iteration loop while tuning the wave ribbon specifically.

## What it shows

Driven entirely by `org.myna.Dictation` (served by `myna-desktop --dbus`):

- **Idle**: nothing — push-to-talk, no persistent overlay.
- **Loading / Recording / Transcribing / Finishing**: the pill with a filled
  mic icon and a state label.
- **Recoverable notice** (e.g. "No speech detected"): a non-blocking pill that
  auto-dismisses after ~3.5 s; a new session can start immediately.
- **Critical error** (e.g. "Microphone unavailable"): a persistent pill with a
  mic-with-slash icon and a dismiss (×) control — reactive but never
  keyboard-focusable, so dismissing it can never steal focus.
- **Audio level**: a flowing, accent-colored wave ribbon (2026-07-30 redesign
  — see `ribbon.js`) calibrated to real speech levels (not a raw linear
  gain): it unfolds when a session starts, flows with your voice, relaxes to
  a thin idle line on a pause, and morphs into a simplified processing
  motion when you stop. Colored from your system's accent-color preference,
  or Ubuntu orange if you haven't set one; falls back to a static line if
  you have reduced motion enabled.

## Layout

- `extension.js` — entry point: wires `dbus.js` → `states.js` → `view.js`.
- `dbus.js` — the `org.myna.Dictation` proxy + name-watch lifecycle. Zero
  Shell dependency (pure `Gio`/`GLib`) — reused verbatim by `dev-lab/`.
- `states.js` — pure wire-state → descriptor mapping (`{key, statusText,
  severity, hidden}`); the stable, unit-tested contract layer.
- `view.js` — the `IndicatorView` seam. A redesign replaces one file
  (`hud.js`) and this factory; nothing else moves.
- `hud.js` + `hudLogic.js` — the current view: `hud.js` is the Shell/Clutter
  actor; `hudLogic.js` is the pure, unit-tested logic factored out of it
  (icon/colour choice, auto-dismiss/replace-in-place rules, and which state
  transitions force a wave-ribbon phase change). Placement is not in either
  file any more: the pill is bottom-centred declaratively by a
  `Layout.MonitorConstraint` plus `stylesheet.css`'s `margin-bottom`.
- `vumeter.js` — pure RMS/peak → calibrated loudness envelope + stale-decay;
  reused unchanged by `ribbon.js`.
- `ribbon.js` — pure wave-ribbon strand/control-point generation and the 5
  lifecycle-phase timing functions (unfold/flow/morph/complete).
- `accent.js` — legacy pure palette helper retained for the standalone
  `dev-lab/`; the shipped Shell HUD uses native `St.Settings` and CSS accent
  colours instead.
- `ribbonPaint.js` — the shared Cairo drawing function, toolkit-agnostic
  (no Shell/Gtk import) — used unmodified by both `hud.js` and `dev-lab/`.
  Also owns the **shared tuning tables** (gradient stops, glow/feather
  passes, billow/taper shapes, per-role thickness and alpha) that the GPU
  path bakes into its shader, so both renderers are driven by one set of
  numbers.
- `ribbonGlsl.js` — **generates** the GPU path's GLSL fragment shader from
  those same tables (the default renderer, see below). A generator rather
  than a `.glsl` file so the constants are read from the one place they are
  defined; no build step, it is an ordinary ES module returning a string.
- `ribbonShader.js` — `ShaderRibbonActor`, a `Clutter.ShaderEffect`-based
  drop-in for `hud.js`'s Cairo `WaveRibbonActor`, exposing the identical
  API. The shipped default; Shell-only (see below).
- `stylesheet.css` — pill/icon/label/ribbon styling, including the severity
  and high-contrast colour classes.
- `dev-lab/` — a standalone GTK4+libadwaita tuning app for the Cairo wave
  ribbon, **not part of the shipped bundle** (see `dev-lab/README.md`).
- `dev-lab-gpu/` — the same for the GPU renderer: a Python `Gtk.GLArea` lab
  plus a headless, display-free render check that compiles and rasterizes
  the generated shader on a real driver. It can also publish
  `org.myna.Dictation` itself, so the sliders drive the **real HUD** in a
  live session without a microphone or a model. Python only because the raw
  GL entry points a standalone GL area needs are not introspectable and so
  are unreachable from gjs; JS still owns the shader, the model and the
  uniform packing, handed over as JSON. **Not part of the shipped bundle**
  (see `dev-lab-gpu/README.md`).
- `test/*.test.js` — headless GJS tests (`gjs -m test/<name>.test.js`) for
  everything above except `hud.js` itself.
- `test/gpu-probe.js` — checks the GPU path's toolkit API is reachable and
  that Cogl accepts the generated shader. Needs mutter's typelibs, so it is
  run manually rather than as part of the headless suite.
- `test/entrance-visual.sh` + `test/visual-driver/` - `hud.js`'s
  *presentation*, driven against a real headless GNOME Shell. **Not part of
  the shipped bundle.**

## GPU rasterization

The Shell HUD rasterizes the ribbon on the GPU (`ribbonShader.js`) as a
per-pixel distance field. Fall back to the Cairo painter with an environment
variable:

```sh
MYNA_SHELL_CAIRO_RIBBON=1
```

**Cairo remains the reference implementation**, and is kept rather than
deleted because:

- **GNOME Shell 50 has no GPU path at all.** `ribbonShader.js` overrides
  `ClutterShaderEffectClass::get_static_snippet`, which arrived in mutter
  51.alpha (`2d5bc0fbff`, "clutter/shader-effect: Port to CoglSnippet"); the
  same commit added `clutter_shader_effect_set_uniform_float`, the only
  introspectable way to push a `vec2`/`vec3`/`vec4` from GJS. On mutter 50
  neither exists, so `hud.js` selects Cairo automatically —
  `ribbonShaderSupported()` registers the effect subclass behind a try/catch
  and logs once when it cannot. That registration is deliberately *lazy*: at
  module scope the throw would abort the `import` and take the whole
  extension down, before `MYNA_SHELL_CAIRO_RIBBON` could ever be read.
- `dev-lab/` cannot use the GPU path. GTK4's `GskGLShader` was deprecated in
  4.16 and no longer renders at all — both its Cairo and GPU paths now fill
  the node with hot pink (`#FF69B4`) as a "missing shader" marker. Its
  replacement, `GtkGLArea`, needs raw `epoxy`/`glCreateShader` calls that
  are not introspectable and so are unreachable from `gjs`. The Cairo
  painter is therefore the only renderer the tuning app can share.
- The headless tests paint into a real `Cairo.ImageSurface`; GLSL has no
  equivalent that runs without a GL context.
- On llvmpipe (VMs, some installs) the "GPU" path is still the CPU, so the
  fallback is also the escape hatch if the shader ever misbehaves on a
  particular driver.

Only *rasterization* moves. `computeRibbonModel` — the phase state machine,
the envelope smoothing, the amplitude response curve — stays pure JS and
stays the single authority for what to draw. The shader regenerates each
strand's sine analytically from the parameters the model now reports
(`amplitude`/`phaseOffset`/`delayMs`/`speedScale`) rather than from
constants of its own, and `test/ribbonGlsl.test.js` asserts both that those
regenerated points match the model's own and that every `#define` still
equals its JS original — so a retune of either renderer cannot silently
desynchronize them.

It also gets a *better* result for the soft passes: `paintGlow`'s stacked
strokes exist only because "Cairo has no native blur", and its own comments
note they band visibly on a near-flat curve. The shader evaluates them as
summed Gaussians, which is what the stack was approximating.

`Shell.GLSLEffect` is **not** used — it was removed from gnome-shell in
`30f545eb00` ("Remove GLSLEffect — now that everything uses
ClutterShaderEffect"). `Clutter.ShaderEffect` + `Cogl.Snippet` is the
supported path, and is what the Shell's own `js/ui/lightbox.js` vignette
uses from JS.

## Compositor behaviour

`hud.js` runs on GNOME Shell's single main loop, the loop that composites
every frame, so it follows the same rules `ui/osdWindow.js` does:

- **The actor tree is built when the view is constructed**, at `enable()`,
  and reused for every session. `show()`/`hide()` only fade opacity and flip
  `visible`, and a `show()` landing inside the 200 ms fade-out picks the pill
  up where that fade left it rather than re-running the entrance over an
  actor the user can still see. Building it on the first `show()` instead put
  actor construction, a GSettings open and a full CSS resolve in the very
  frame the pill was trying to appear in; `ui/osdWindow.js` builds its OSD at
  startup for the same reason.
- **No overshooting easing mode on `opacity`.** Clutter's animatable path
  feeds the interpolated value through `g_value_get_uint` into a `guint8`
  setter, which truncates rather than clamps - so an `EASE_OUT_BACK` peak of
  ~280 wraps to 24 and blanks the actor. Scale is a double and overshoots
  safely, so the entrance eases the two on separate modes.
- **The ribbon animates off the actor's frame clock** (a `Clutter.Timeline`
  bound to the actor), not a `GLib.timeout_add`. A fixed 24 Hz timer against
  a 60 Hz output beats against vsync and reads as juddering motion. The
  timeline also idles automatically whenever the ribbon is unmapped, so a
  hidden HUD and a critical error (which hides the ribbon) both cost nothing.
- **Raised above its chrome siblings on every present.** Chrome paints in
  insertion order, and the Ubuntu dock re-adds itself on every re-track, so
  landing above it once proves nothing. With a bottom dock in its
  non-reserving (intellihide) state the two overlap, and without the raise the
  pill is completely hidden behind the dock. `osdWindow.js` raises itself the
  same way, for the same reason. Placement stays on the *work area*, so a dock
  that does reserve space is cleared rather than drawn over.
- **`global.compositor.disable_unredirect()` while the pill is on screen**,
  balanced on hide. Over a fullscreen window mutter may scan the window out
  directly, and an overlay appearing forces it in and out of that path.
- **Nothing per-frame that isn't drawing.** `St.Settings` invalidates the
  native CSS accent colours and reduced-motion state only when they change,
  and `_applyDescriptor` only writes an icon name, label or style class when
  it actually changed (each write invalidates St's theme node).
- **No synchronous D-Bus.** `dbus.js` builds its proxy with
  `Gio.DBusProxy.new`, cancelling an in-flight construction on `disable()`.
  The `new_sync` it replaced blocked the whole desktop on the daemon's
  initial `GetAll`, at exactly the moment the pill was about to appear.

Verified against a real GNOME Shell rather than asserted, and the parts of
that which are mechanical now run as `test/entrance-visual.sh` (below).

## Testing

```sh
cd extensions/myna-shell
test/run-suite.sh          # everything below, in one go
```

`test/run-suite.sh` runs the pure GJS suites (`test/*.test.js`, no Shell),
then `test/gpu-probe.sh` (mutter's typelibs), then `test/entrance-visual.sh`
(a real headless Shell). The last two exit 77 when they cannot judge, which
the runner treats as a skip.

It runs in CI as `make test-extension`, in its own Workshop
(`.workshop/myna-shell.yaml`) rather than the main one. A Workshop SDK cannot
carry its own base image, so the Shell version a test can reach comes from the
workshop's base: the main workshop sits on ubuntu@24.04 because that is the
snap's `core24`, and no 24.04 archive has - or can have - the Shell this
extension targets. Until the extension is ported backwards, the two need
different bases.

Pure logic (`states.js`, `vumeter.js`, `ribbon.js`, `accent.js`,
`hudLogic.js`, `dbus.js`'s lifecycle) is unit-tested headless — including a
real headless-Cairo smoke check of `ribbonPaint.js` (an `ImageSurface`
needs no display server). The widget tree in `hud.js` cannot be reached that
way: GNOME Shell's Clutter fork aborts if you construct an actor outside a
running compositor.

So `test/entrance-visual.sh` brings a compositor. It stands up a headless
GNOME Shell on a private bus with a virtual monitor, loads a driver
(`test/visual-driver/`) that builds the real `HudView` out of the working
tree, and samples the pill's opacity, visibility and scale once per presented
frame. What it asserts is *presentation*: that the pill is built before it is
needed, that its entrance never blanks, and that a session restarting inside
the previous one's fade-out is picked up rather than re-entered. Everything
private, torn down on exit, and safe to run on a desktop - it never touches
the caller's session or dconf.

It skips (exit 77) rather than failing where no Shell can run, or where the
Shell is too starved to present enough frames to judge - an animation seen at
three frames could hide a one-frame blank between them, and a guess there is
worse than an honest skip.

Geometry and colour stay manual-acceptance; see
`specs/004-gnome-shell-indicator/quickstart.md`.

## Out of scope

Text injection, model/mic selection, translation, transcript display, and
screen-reader announcements (tracked separately, plan T56) are all out of
scope for this extension. Public distribution (extensions.gnome.org review,
Ubuntu archive, or bundling in a snap) is noted as follow-up, not delivered
here — install today by copying the bundle in-tree per above.
