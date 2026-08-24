# GPU ribbon dev-lab

A standalone lab for the **GPU** wave-ribbon renderer (`../ribbonShader.js`
and the GLSL `../ribbonGlsl.js` generates), the counterpart to `../dev-lab/`
which does the same job for the Cairo painter.

**Not part of the shipped extension.** Nothing in the extension imports it,
and it is never installed.

```sh
python3 main.py                      # interactive lab
python3 render_headless.py           # no display needed; exits non-zero on failure
python3 render_headless.py --profile es300 --out /tmp/ribbon.png
python3 dbus_headless.py             # no display needed; exits non-zero on failure
```

## Why this is Python and the other lab is JS

The Shell rasterizes the ribbon with `Clutter.ShaderEffect` + `Cogl.Snippet`.
Neither exists outside GNOME Shell, so tuning the shader used to mean
reloading a live session for every change.

GTK4 has no usable replacement reachable from gjs:

- `GskGLShader` was deprecated in 4.16 and **no longer renders at all** —
  both its Cairo and GPU paths fill the node with hot pink (`#FF69B4`) as a
  "missing shader" marker. It fails silently; there is no warning.
- `GtkGLArea`, its replacement, needs raw `glCreateShader`/`glUseProgram`
  calls. Those come from `libepoxy`, which has no GObject-Introspection
  typelib, so they are unreachable from gjs.

PyOpenGL provides exactly those entry points, so Python can drive a
`Gtk.GLArea`. **That is the only reason this is not JS.**

## JS stays the root of trust

Python renders. It decides nothing.

`bridge.js` runs under a plain `gjs` — importing no Clutter, no St, no Cogl —
and hands over JSON:

| | comes from | Python's role |
|---|---|---|
| shader source | `buildRibbonShader()` | compile it |
| uniform list | `RIBBON_UNIFORMS` | look up locations |
| per-frame values | `computeRibbonModel()` → `packRibbonUniforms()` | upload them |
| accent palette | `accent.js` `SystemPreferences` | nothing — it is already in the uniforms |
| light/dark + reduced motion | GSettings, read JS-side | apply to the window |

Those are the *same functions the Shell extension calls*, so the lab cannot
drift from the shipped renderer: if it looks right here, it is right there.
Adding a uniform or retuning a constant needs no change on this side at all.

```
gjs -m bridge.js --shader     # one JSON object: the generated shader
gjs -m bridge.js --serve      # a JSON line out per JSON line in
```

The desktop preferences ride along on **every** frame, not just at
startup, so changing the system accent or the light/dark preference is
picked up live — the same thing the Shell HUD does through its `changed::`
subscriptions. The lab window's variant is *forced* from the GSettings value
rather than left to `Adw.ColorScheme.DEFAULT`, because DEFAULT asks
libadwaita to detect the preference through the settings portal, which
silently stays light wherever that portal is unavailable (a jhbuild session,
or a login without `xdg-desktop-portal-gnome`).

`--serve` is a long-lived subprocess rather than one process per frame:
startup is ~50ms but a request/reply over the pipe is well under a
millisecond, so slider drags stay responsive while every frame still comes
from the real JS model.

## Driving a live Shell HUD

The lab shows the ribbon. The switch under **GNOME Shell** shows the whole
*HUD*: turn it on and the lab claims `org.myna.Dictation`, the bus name the
extension watches, so a live session draws its real pill — real label, real
icon, real placement, real severity colours — driven by these sliders.

That makes the lab a stand-in for `myna-desktop --dbus`, which is otherwise
the only way to see the HUD react at all, and which needs a microphone, a
loaded model and something worth transcribing before it will show you a
`notice`. Here, `notice` is one combo entry away.

The mapping lives in `dictation_service.py` and is the inverse of the
extension's own `hudLogic.ribbonPhaseForStateKey`:

| lab control | wire property |
|---|---|
| phase `unfold`/`flow`/`relax` | `State = recording` |
| phase `morph` | `State = transcribing` |
| phase `complete` | `State = finalizing` |
| severity `recoverable` | `State = notice` (outranks the phase) |
| severity `critical` | `State = error` + a content-free `ErrorMessage` |
| input level | `AudioRms` / `AudioPeak` |
| the switch, off | the name is released — the extension goes dormant |

The wire is lossy — several states collapse onto `flow` — so the phase row
says what the Shell will actually render for each entry. Every phase
round-trips today, except `unfold`: the Shell does play it, but on its own
clock when the pill appears, rather than because anything on the wire asked
for it. A phase added to `ribbon.js` that no dictation state requests would
not round-trip at all, and the row would say so instead of leaving it
looking like a broken bridge. (`relax` was such a phase, and was removed
from `ribbon.js` once the lab made its unreachability visible.)

Two details that are easy to get wrong and would make the Shell disagree
with the lab's own window:

- **The level slider is not the wire value.** The slider is the *smoothed
  envelope* the ribbon consumes, while the wire carries raw RMS, which the
  extension pushes back through `vumeter.js`'s calibrated dBFS curve.
  `envelope_to_levels()` inverts that curve so both ribbons sit at the same
  amplitude for the same setting; `dbus_headless.py` checks the round trip
  against a transcribed copy of `boostLevel`, so the constants cannot drift
  apart unnoticed.
- **Levels stop when recording stops**, rather than being held or zeroed
  every tick. That is what a real daemon does — nothing is captured during
  `transcribing` — and it lets the extension's stale-decay ease the VU to
  its floor exactly as it does at the end of a real session.

Updates go out at 20 Hz, matching the contract's ~15–20 Hz cadence rather
than the lab's ~60 fps render loop, so the extension sees the update rate it
was tuned against.

The name is **never** taken by force (`BusNameOwnerFlags.NONE`): if
`myna-desktop` is already running, the row says so instead of silently
displacing the real daemon. The interface's `Start`/`Stop`/`Toggle` methods
are implemented too — the extension never calls them, but they make the lab
drivable from a terminal, and `Stop` publishes `idle`, which is the case
that clears the pill entirely:

```sh
gdbus call --session -d org.myna.Dictation \
    -o /org/myna/Dictation -m org.myna.Dictation.Toggle
```

## `render_headless.py`

`../test/ribbonGlsl.test.js` already checks the generated GLSL with
`glslangValidator` — but that is a *parser*. This runs the shader through a
real driver and rasterizes a frame, catching what a parser cannot:

- a uniform that fails to link, or that the shader never reads and the
  linker therefore discards (a silent tuning bug — the value is uploaded,
  ignored, and nothing complains);
- a shader that compiles perfectly and draws nothing;
- a distance field that floods the whole canvas instead of drawing a band;
- phases that all render identically, which every per-frame check would
  otherwise happily pass.

A **surfaceless EGL** context needs no display server, no window and no
Shell, so this runs over SSH and in CI. On llvmpipe the "GPU" is software,
but the GLSL compiler is the same Mesa one.

### Two GLSL profiles

| `--profile` | front-end | who gets it |
|---|---|---|
| `gl120` (default) | desktop GLSL 1.20 | surfaceless EGL |
| `es300` | GLSL ES 3.00 | `Gtk.GLArea` — GTK4 requests GLES 3.2 and refuses immediate mode |

Running both is not just a compatibility tax: it exercises the generated
shader on two quite different GLSL front-ends, for the same reason the JS
test runs glslang for 1.20 *and* ES 1.00. It also covers the interactive
lab's shader variant without needing a window.

`buildRibbonShader()` returns a Cogl *snippet* — a declarations block plus a
body assigning `cogl_color_out` — which Cogl normally splices into a program
it generates itself. Outside the Shell there is no Cogl, so `ribbon_gl.py`
supplies the surrounding declarations, mirroring `standaloneShader()` in the
JS test.

## Files

- `bridge.js` — the JS→JSON seam. The only file that imports the extension.
- `bridge.py` — the Python end of that pipe.
- `ribbon_gl.py` — compile, upload, draw. Knows nothing about ribbons.
- `main.py` — the interactive `Gtk.GLArea` lab.
- `render_headless.py` — the surfaceless render check.
- `dictation_service.py` — the `org.myna.Dictation` publisher and the
  lab-look → wire-state mapping.
- `dbus_headless.py` — checks that publisher against the contract on a
  private bus of its own, so it can never collide with a real
  `myna-desktop`.

## Requirements

`gjs` and GTK4 + libadwaita introspection are already needed by the rest of
the extension. Beyond those:

```sh
pip install -r requirements.txt
```

`PyOpenGL` is a pure-ctypes wrapper (nothing to compile). `Pillow` is only
needed for `--out`.
