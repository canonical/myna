"""ribbon_gl.py — compile and draw the JS-generated ribbon shader.

Shared by the interactive lab (`main.py`) and the headless render check
(`render_headless.py`), so both put exactly the same GLSL through exactly
the same uniform upload.

Nothing here knows what a ribbon *is*. The shader source, the uniform list
and the per-frame values all arrive from `bridge.js`; this module's only job
is to hand them to the driver. Keeping it that dumb is the point — see
README.md.

Two profiles are supported, because the two callers get different contexts
and neither choice was ours to make:

  - ``PROFILE_GL120`` — desktop GL 1.20, what a surfaceless EGL context
    gives by default. Immediate mode, ``gl_FragColor``.
  - ``PROFILE_ES300`` — GLES 3.00, what GTK4 hands a ``Gtk.GLArea`` (it
    requests GLES 3.2 here, and refuses immediate mode outright). VAO/VBO,
    an explicit ``out`` colour.

Supporting both is not merely a compatibility tax: it exercises the
generated shader on two quite different GLSL front-ends, for the same reason
`test/ribbonGlsl.test.js` runs glslang for 1.20 *and* ES 1.00.
"""

from __future__ import annotations

import ctypes

import numpy as np
from OpenGL import GL

PROFILE_GL120 = "gl120"
PROFILE_ES300 = "es300"

# `buildRibbonShader()` returns a Cogl *snippet*: a declarations block and a
# body assigning `cogl_color_out`, both of which Cogl normally splices into a
# program it generates itself. Outside the Shell there is no Cogl, so the
# surrounding declarations are supplied here — mirroring `standaloneShader()`
# in test/ribbonGlsl.test.js, which does the same for glslangValidator.
_VERTEX = {
    PROFILE_GL120: """#version 120
void main() {
    gl_TexCoord[0] = gl_MultiTexCoord0;
    gl_Position = gl_Vertex;
}
""",
    PROFILE_ES300: """#version 300 es
in vec2 aPosition;
out vec2 vUv;
void main() {
    vUv = aPosition;
    gl_Position = vec4(aPosition * 2.0 - 1.0, 0.0, 1.0);
}
""",
}

_FRAGMENT = {
    PROFILE_GL120: """#version 120
vec4 cogl_color_out;
vec4 cogl_color_in;
vec4 cogl_tex_coord_in[4];
%(declarations)s
void main() {
cogl_tex_coord_in[0] = gl_TexCoord[0];
%(code)s
gl_FragColor = cogl_color_out;
}
""",
    PROFILE_ES300: """#version 300 es
precision highp float;
in vec2 vUv;
out vec4 fragColor;
vec4 cogl_color_out;
vec4 cogl_color_in;
vec4 cogl_tex_coord_in[4];
%(declarations)s
void main() {
cogl_tex_coord_in[0] = vec4(vUv, 0.0, 1.0);
%(code)s
fragColor = cogl_color_out;
}
""",
}


class ShaderError(RuntimeError):
    """A compile or link failure, carrying the driver's own log."""


def build_fragment_source(shader: dict, profile: str) -> str:
    """Wrap the Cogl snippet into a complete, standalone fragment shader.

    :param shader: the ``--shader`` JSON from ``bridge.js``.
    :param profile: ``PROFILE_GL120`` or ``PROFILE_ES300``.
    """
    return _FRAGMENT[profile] % {
        "declarations": shader["declarations"],
        "code": shader["code"],
    }


def _compile(source: str, kind) -> int:
    shader = GL.glCreateShader(kind)
    GL.glShaderSource(shader, source)
    GL.glCompileShader(shader)
    if GL.glGetShaderiv(shader, GL.GL_COMPILE_STATUS) != GL.GL_TRUE:
        raise ShaderError(GL.glGetShaderInfoLog(shader).decode(errors="replace"))
    return shader


def build_program(shader: dict, profile: str = PROFILE_GL120) -> int:
    """Compile and link the ribbon program on the current GL context.

    :raises ShaderError: with the driver's log, which is the only place a
        generator bug (a missing decimal point, an undeclared identifier)
        surfaces as a message rather than as a blank ribbon.
    """
    program = GL.glCreateProgram()
    GL.glAttachShader(program, _compile(_VERTEX[profile], GL.GL_VERTEX_SHADER))
    GL.glAttachShader(
        program,
        _compile(build_fragment_source(shader, profile), GL.GL_FRAGMENT_SHADER))
    if profile == PROFILE_ES300:
        GL.glBindAttribLocation(program, 0, "aPosition")
    GL.glLinkProgram(program)
    if GL.glGetProgramiv(program, GL.GL_LINK_STATUS) != GL.GL_TRUE:
        raise ShaderError(GL.glGetProgramInfoLog(program).decode(errors="replace"))
    return program


_SETTERS = {
    1: GL.glUniform1fv,
    2: GL.glUniform2fv,
    3: GL.glUniform3fv,
    4: GL.glUniform4fv,
}


def upload_uniforms(program: int, uniform_specs: list, values: dict) -> list:
    """Upload one frame.

    :param uniform_specs: ``RIBBON_UNIFORMS`` as exported by the bridge.
    :param values: ``packRibbonUniforms`` output for this frame.
    :returns: names the linker discarded. A uniform the shader never reads
        is optimized out and has no location, which is not an error — but it
        is worth surfacing, because a uniform that was *supposed* to be read
        and silently is not looks identical to a tuning mistake.
    """
    optimized_out = []
    for spec in uniform_specs:
        name, components = spec["name"], spec["components"]
        location = GL.glGetUniformLocation(program, name)
        if location < 0:
            optimized_out.append(name)
            continue
        _SETTERS[components](location, 1, values[name])
    return optimized_out


# The full-canvas quad the fragment shader rasterizes over, as a strip.
_QUAD = np.array([0, 0, 1, 0, 0, 1, 1, 1], dtype=np.float32)


class QuadDrawer:
    """Draws the full-canvas quad, however the active profile requires.

    An object rather than a function because the ES path owns a VAO and a
    VBO that must outlive a single frame; the 1.20 path owns nothing and
    just replays immediate-mode calls.
    """

    def __init__(self, profile: str = PROFILE_GL120) -> None:
        self.profile = profile
        self._vao = None
        self._vbo = None
        if profile != PROFILE_ES300:
            return
        self._vao = GL.glGenVertexArrays(1)
        GL.glBindVertexArray(self._vao)
        self._vbo = GL.glGenBuffers(1)
        GL.glBindBuffer(GL.GL_ARRAY_BUFFER, self._vbo)
        GL.glBufferData(GL.GL_ARRAY_BUFFER, _QUAD.nbytes, _QUAD, GL.GL_STATIC_DRAW)
        GL.glEnableVertexAttribArray(0)
        GL.glVertexAttribPointer(0, 2, GL.GL_FLOAT, GL.GL_FALSE, 0,
                                 ctypes.c_void_p(0))
        GL.glBindVertexArray(0)

    def draw(self) -> None:
        if self.profile == PROFILE_ES300:
            GL.glBindVertexArray(self._vao)
            GL.glDrawArrays(GL.GL_TRIANGLE_STRIP, 0, 4)
            GL.glBindVertexArray(0)
            return
        GL.glBegin(GL.GL_QUADS)
        for x, y in ((0, 0), (1, 0), (1, 1), (0, 1)):
            GL.glMultiTexCoord2f(GL.GL_TEXTURE0, x, y)
            GL.glVertex2f(x * 2 - 1, y * 2 - 1)
        GL.glEnd()
