#!/usr/bin/env python3
"""render_headless.py — compile and render the ribbon shader with no display.

    python3 render_headless.py [--out ribbon.png]

`test/ribbonGlsl.test.js` already checks the generated GLSL with
glslangValidator, which is a *parser*. This runs it through a real driver
and then actually rasterizes a frame, which catches the two things a parser
cannot:

  - a uniform that fails to link, or that the shader never reads and the
    linker therefore discards (a silent tuning bug: the value is uploaded,
    ignored, and nothing complains);
  - a shader that compiles perfectly and draws nothing.

An EGL surfaceless context needs no display server, no window and no Shell,
so this runs anywhere — including over SSH and on llvmpipe in CI, where the
"GPU" is software but the GLSL compiler is the same Mesa one.

Exits non-zero on failure, so it can be wired into a test target as-is.
"""

from __future__ import annotations

import argparse
import ctypes
import sys

import numpy as np
from OpenGL import EGL, GL

from bridge import RibbonModel, load_shader
from ribbon_gl import (
    PROFILE_ES300,
    PROFILE_GL120,
    QuadDrawer,
    ShaderError,
    build_program,
    upload_uniforms,
)

WIDTH, HEIGHT = 360, 32

failures = 0


def check(name: str, condition: bool, detail: str = "") -> bool:
    global failures
    if condition:
        print(f"ok   {name}" + (f" ({detail})" if detail else ""))
    else:
        failures += 1
        print(f"FAIL {name}" + (f" ({detail})" if detail else ""))
    return condition


def make_context():
    """A surfaceless EGL context: no display server, no window."""
def make_context(profile: str):
    """A surfaceless EGL context: no display server, no window.

    :param profile: which GLSL front-end to exercise. ``PROFILE_ES300``
        matches what GTK4 gives the interactive lab, so running both here
        covers the lab's shader variant without needing a window.
    """
    is_es = profile == PROFILE_ES300
    display = EGL.eglGetDisplay(EGL.EGL_DEFAULT_DISPLAY)
    major, minor = ctypes.c_long(), ctypes.c_long()
    if not EGL.eglInitialize(display, major, minor):
        raise RuntimeError("eglInitialize failed — no usable EGL driver")
    configs = (EGL.EGLConfig * 1)()
    count = ctypes.c_long()
    attributes = [
        EGL.EGL_SURFACE_TYPE, EGL.EGL_PBUFFER_BIT,
        EGL.EGL_RENDERABLE_TYPE,
        EGL.EGL_OPENGL_ES3_BIT if is_es else EGL.EGL_OPENGL_BIT,
        EGL.EGL_RED_SIZE, 8, EGL.EGL_GREEN_SIZE, 8,
        EGL.EGL_BLUE_SIZE, 8, EGL.EGL_ALPHA_SIZE, 8,
        EGL.EGL_NONE,
    ]
    if not EGL.eglChooseConfig(display, attributes, configs, 1, count) or count.value < 1:
        raise RuntimeError("no EGL config with an alpha channel")
    EGL.eglBindAPI(EGL.EGL_OPENGL_ES_API if is_es else EGL.EGL_OPENGL_API)
    context_attributes = (
        [EGL.EGL_CONTEXT_CLIENT_VERSION, 3, EGL.EGL_NONE] if is_es else None)
    context = EGL.eglCreateContext(
        display, configs[0], EGL.EGL_NO_CONTEXT, context_attributes)
    if not context:
        raise RuntimeError("eglCreateContext failed")
    EGL.eglMakeCurrent(display, EGL.EGL_NO_SURFACE, EGL.EGL_NO_SURFACE, context)
    return display


def make_target(width: int, height: int):
    """An RGBA framebuffer to render into, since there is no window."""
    texture = GL.glGenTextures(1)
    GL.glBindTexture(GL.GL_TEXTURE_2D, texture)
    GL.glTexImage2D(GL.GL_TEXTURE_2D, 0, GL.GL_RGBA8, width, height, 0,
                    GL.GL_RGBA, GL.GL_UNSIGNED_BYTE, None)
    framebuffer = GL.glGenFramebuffers(1)
    GL.glBindFramebuffer(GL.GL_FRAMEBUFFER, framebuffer)
    GL.glFramebufferTexture2D(GL.GL_FRAMEBUFFER, GL.GL_COLOR_ATTACHMENT0,
                              GL.GL_TEXTURE_2D, texture, 0)
    status = GL.glCheckFramebufferStatus(GL.GL_FRAMEBUFFER)
    if status != GL.GL_FRAMEBUFFER_COMPLETE:
        raise RuntimeError(f"incomplete framebuffer: {status}")


def render(program, drawer, shader, values, width, height):
    """Render one frame and read it back, top row first."""
    GL.glViewport(0, 0, width, height)
    GL.glClearColor(0.0, 0.0, 0.0, 0.0)
    GL.glClear(GL.GL_COLOR_BUFFER_BIT)
    GL.glUseProgram(program)
    optimized_out = upload_uniforms(program, shader["uniforms"], values)
    drawer.draw()
    GL.glFinish()
    pixels = GL.glReadPixels(0, 0, width, height, GL.GL_RGBA, GL.GL_UNSIGNED_BYTE)
    # GL's origin is bottom-left; flip so row 0 is the top, as every image
    # format and every human expects.
    image = np.frombuffer(pixels, dtype=np.uint8).reshape(height, width, 4)[::-1]
    return image, optimized_out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", metavar="PNG",
                        help="also write a contact sheet of every phase here")
    parser.add_argument("--profile", choices=[PROFILE_GL120, PROFILE_ES300],
                        default=PROFILE_GL120,
                        help="which GLSL front-end to compile against "
                             "(es300 is what the interactive lab gets)")
    args = parser.parse_args()

    make_context(args.profile)
    print(f"     renderer: {GL.glGetString(GL.GL_RENDERER).decode()}"
          f"  |  {GL.glGetString(GL.GL_SHADING_LANGUAGE_VERSION).decode()}")

    shader = load_shader()
    try:
        program = build_program(shader, args.profile)
    except ShaderError as error:
        check("the generated shader compiles and links on a real driver", False)
        print(error)
        return 1
    check("the generated shader compiles and links on a real driver", True)

    make_target(WIDTH, HEIGHT)
    drawer = QuadDrawer(args.profile)

    with RibbonModel() as model:
        # Every phase, because a shader can easily draw one state correctly
        # and leave another blank — the morph phase in particular takes a
        # path (the travelling dots) that flow never touches.
        frames = {}
        for phase in shader["phases"]:
            response = model.frame(
                width=WIDTH, height=HEIGHT, envelope=0.7, elapsedMs=1200,
                phase=phase, phaseElapsedMs=100,
            )
            image, optimized_out = render(
                program, drawer, shader, response["uniforms"], WIDTH, HEIGHT)
            frames[phase] = image

            check(f"{phase}: every uniform survived linking",
                  not optimized_out, ", ".join(optimized_out) or "none discarded")

            lit = int((image[:, :, 3] > 8).sum())
            check(f"{phase}: the frame is not blank", lit > 0, f"{lit} lit pixels")

            # A ribbon occupies a band, not the whole canvas. Catching
            # "everything is filled" matters because a broken distance field
            # tends to fail that way rather than by drawing nothing.
            covered = lit / (WIDTH * HEIGHT)
            check(f"{phase}: the frame is a ribbon, not a full flood",
                  covered < 0.98, f"{covered:.0%} covered")

            check(f"{phase}: nothing rendered outside the 0-255 range",
                  bool(image.min() >= 0 and image.max() <= 255))

        # The phases must not all look the same: a uniform that silently
        # fails to reach the shader shows up here as identical frames, which
        # every per-frame check above would happily pass.
        distinct = {phase: image.tobytes() for phase, image in frames.items()}
        check("the phases render as visibly different frames",
              len(set(distinct.values())) == len(frames),
              f"{len(set(distinct.values()))} distinct of {len(frames)}")

    if args.out:
        from PIL import Image
        # A contact sheet rather than one phase, so a glance shows the whole
        # lifecycle and how each state differs from its neighbours.
        gap = 4
        sheet = np.zeros(
            (len(frames) * (HEIGHT + gap) - gap, WIDTH, 4), dtype=np.uint8)
        for index, phase in enumerate(shader["phases"]):
            top = index * (HEIGHT + gap)
            sheet[top:top + HEIGHT] = frames[phase]
        Image.fromarray(sheet).save(args.out)
        print(f"     wrote {args.out} ({', '.join(shader['phases'])}, top to bottom)")

    print("PASS render_headless.py" if failures == 0
          else f"FAIL render_headless.py ({failures})")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
