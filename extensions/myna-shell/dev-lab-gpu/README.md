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

## Requirements

`gjs` and GTK4 + libadwaita introspection are already needed by the rest of
the extension. Beyond those:

```sh
pip install -r requirements.txt
```

`PyOpenGL` is a pure-ctypes wrapper (nothing to compile). `Pillow` is only
needed for `--out`.
