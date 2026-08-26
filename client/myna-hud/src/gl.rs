//! gl — the ribbon's GPU renderer (feature 004, T121; research R23/R24).
//!
//! The Shell extension drew the ribbon through a Clutter effect whose Cogl
//! snippet came from [`crate::shader`]; standing alone we compile that same
//! snippet ourselves and draw it over a fullscreen quad inside a
//! [`gtk4::GLArea`].
//!
//! ## Why raw `epoxy_*` statics rather than the `gl` crate's loader
//!
//! GTK itself dispatches GL through libepoxy, which exports every entry
//! point as a *function pointer variable* (`epoxy_glCreateShader`, …) that
//! its C headers dereference. Ubuntu's libepoxy exports **no** generic
//! `epoxy_get_proc_address`, so `gl::load_with` has nothing to load from
//! (T101's discovery). Declaring the pointers as `extern` statics is both
//! the supported ABI and a bonus: epoxy resolves each to the right entry
//! point for the *current* context's API, so one binary serves desktop GL
//! and GLES without a compile-time choice. The `gl` crate is kept for its
//! types and constants only.
//!
//! ## Profile
//!
//! GTK hands us an OpenGL **ES 3.2** context even under X11/xvfb (T101), so
//! [`GlProfile::Es300`] is the production path; the desktop 1.20 profile is
//! retained because the shader generator still targets it and the lab may
//! run on a compatibility context.

use std::ffi::{CStr, CString};
use std::os::raw::c_char;

use crate::ribbon::RibbonModel;
use crate::shader::{
    build_ribbon_shader, pack_ribbon_uniforms, standalone_shader, vertex_shader, GlProfile,
    RibbonPalette,
};

#[allow(non_snake_case)]
mod epoxy {
    use gl::types::*;
    use std::os::raw::c_char;

    macro_rules! epoxy_fns {
        ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?;)*) => {
            #[link(name = "epoxy")]
            extern "C" {
                $(pub static $name: unsafe extern "C" fn($($ty),*) $(-> $ret)?;)*
            }
        };
    }

    epoxy_fns! {
        fn epoxy_glCreateShader(shaderType: GLenum) -> GLuint;
        fn epoxy_glShaderSource(shader: GLuint, count: GLsizei, string: *const *const c_char, length: *const GLint);
        fn epoxy_glCompileShader(shader: GLuint);
        fn epoxy_glGetShaderiv(shader: GLuint, pname: GLenum, params: *mut GLint);
        fn epoxy_glGetShaderInfoLog(shader: GLuint, bufSize: GLsizei, length: *mut GLsizei, infoLog: *mut c_char);
        fn epoxy_glDeleteShader(shader: GLuint);
        fn epoxy_glAttachShader(program: GLuint, shader: GLuint);
        fn epoxy_glCreateProgram() -> GLuint;
        fn epoxy_glLinkProgram(program: GLuint);
        fn epoxy_glBindAttribLocation(program: GLuint, index: GLuint, name: *const c_char);
        fn epoxy_glGetProgramiv(program: GLuint, pname: GLenum, params: *mut GLint);
        fn epoxy_glGetProgramInfoLog(program: GLuint, bufSize: GLsizei, length: *mut GLsizei, infoLog: *mut c_char);
        fn epoxy_glUseProgram(program: GLuint);
        fn epoxy_glDeleteProgram(program: GLuint);
        fn epoxy_glGetUniformLocation(program: GLuint, name: *const c_char) -> GLint;
        fn epoxy_glUniform1fv(location: GLint, count: GLsizei, value: *const GLfloat);
        fn epoxy_glUniform2fv(location: GLint, count: GLsizei, value: *const GLfloat);
        fn epoxy_glUniform3fv(location: GLint, count: GLsizei, value: *const GLfloat);
        fn epoxy_glUniform4fv(location: GLint, count: GLsizei, value: *const GLfloat);
        fn epoxy_glGenVertexArrays(n: GLsizei, arrays: *mut GLuint);
        fn epoxy_glBindVertexArray(array: GLuint);
        fn epoxy_glDeleteVertexArrays(n: GLsizei, arrays: *const GLuint);
        fn epoxy_glGenBuffers(n: GLsizei, buffers: *mut GLuint);
        fn epoxy_glBindBuffer(target: GLenum, buffer: GLuint);
        fn epoxy_glBufferData(target: GLenum, size: GLsizeiptr, data: *const std::ffi::c_void, usage: GLenum);
        fn epoxy_glDeleteBuffers(n: GLsizei, buffers: *const GLuint);
        fn epoxy_glEnableVertexAttribArray(index: GLuint);
        fn epoxy_glVertexAttribPointer(index: GLuint, size: GLint, type_: GLenum, normalized: GLboolean, stride: GLsizei, pointer: *const std::ffi::c_void);
        fn epoxy_glDrawArrays(mode: GLenum, first: GLint, count: GLsizei);
        fn epoxy_glViewport(x: GLint, y: GLint, width: GLsizei, height: GLsizei);
        fn epoxy_glClearColor(red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat);
        fn epoxy_glClear(mask: GLbitfield);
        fn epoxy_glEnable(cap: GLenum);
        fn epoxy_glDisable(cap: GLenum);
        fn epoxy_glBlendFunc(sfactor: GLenum, dfactor: GLenum);
        fn epoxy_glGetString(name: GLenum) -> *const GLubyte;
        fn epoxy_glReadPixels(x: GLint, y: GLint, width: GLsizei, height: GLsizei, format: GLenum, type_: GLenum, pixels: *mut std::ffi::c_void);
    }
}

/// Read the current framebuffer back as RGBA8, row-major from the bottom
/// left (GL's origin). Used by the render check (`examples/render_check.rs`)
/// to assert the ribbon actually lit pixels — the one failure mode that
/// compiles cleanly and shows as an empty overlay.
///
/// # Safety
/// A GL context must be current.
pub unsafe fn read_pixels(width: i32, height: i32) -> Vec<u8> {
    let mut buffer = vec![0u8; (width * height * 4) as usize];
    (epoxy::epoxy_glReadPixels)(
        0,
        0,
        width,
        height,
        gl::RGBA,
        gl::UNSIGNED_BYTE,
        buffer.as_mut_ptr() as *mut std::ffi::c_void,
    );
    buffer
}

/// The quad's corners in `[0, 1]` — [`vertex_shader`] maps them to clip
/// space and passes them through as the UV.
const QUAD: [f32; 12] = [
    0.0, 0.0, 1.0, 0.0, 1.0, 1.0, //
    0.0, 0.0, 1.0, 1.0, 0.0, 1.0,
];

/// The attribute location the quad is bound to (see
/// [`crate::shader::POSITION_ATTRIBUTE`]).
const POSITION_LOCATION: gl::types::GLuint = 0;

/// A compile or link failure, carrying the driver's own log — the only place
/// a generator bug surfaces as a *message* rather than as a blank ribbon.
#[derive(Debug, Clone)]
pub struct ShaderError(pub String);

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ShaderError {}

/// The ribbon program and its quad, owned by a realized GL context.
pub struct RibbonRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    vbo: gl::types::GLuint,
    profile: GlProfile,
}

impl RibbonRenderer {
    /// Detect the current context's profile from `GL_VERSION`.
    ///
    /// GTK gives GLES nearly everywhere now (T101 saw ES 3.2 even on
    /// xvfb/X11); a desktop context falls back to the 1.20 profile the
    /// generator has always targeted.
    pub fn detect_profile() -> GlProfile {
        let version = unsafe {
            let ptr = (epoxy::epoxy_glGetString)(gl::VERSION);
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            }
        };
        if version.contains("OpenGL ES") {
            // "OpenGL ES 3.2 Mesa …" — ES 2.0 would need the ES 1.00
            // profile, but GLArea never requests one that old.
            if version.contains("OpenGL ES 2.") {
                GlProfile::Es100
            } else {
                GlProfile::Es300
            }
        } else {
            GlProfile::Gl120
        }
    }

    /// Compile, link and upload the ribbon program on the *current* context.
    /// Call from `GLArea::connect_realize` after `make_current`.
    pub fn realize() -> Result<Self, ShaderError> {
        let profile = Self::detect_profile();
        let shader = build_ribbon_shader();
        let vertex_source = vertex_shader(profile);
        let fragment_source = standalone_shader(&shader, profile);

        unsafe {
            let vertex = compile(gl::VERTEX_SHADER, &vertex_source)?;
            let fragment = match compile(gl::FRAGMENT_SHADER, &fragment_source) {
                Ok(f) => f,
                Err(e) => {
                    (epoxy::epoxy_glDeleteShader)(vertex);
                    return Err(e);
                }
            };

            let program = (epoxy::epoxy_glCreateProgram)();
            (epoxy::epoxy_glAttachShader)(program, vertex);
            (epoxy::epoxy_glAttachShader)(program, fragment);
            // Bind before linking so the renderer never has to query it.
            let name = CString::new(crate::shader::POSITION_ATTRIBUTE).unwrap();
            (epoxy::epoxy_glBindAttribLocation)(program, POSITION_LOCATION, name.as_ptr());
            (epoxy::epoxy_glLinkProgram)(program);

            // The shaders are reference-counted by the program now.
            (epoxy::epoxy_glDeleteShader)(vertex);
            (epoxy::epoxy_glDeleteShader)(fragment);

            let mut status: gl::types::GLint = 0;
            (epoxy::epoxy_glGetProgramiv)(program, gl::LINK_STATUS, &mut status);
            if status == 0 {
                let log = program_log(program);
                (epoxy::epoxy_glDeleteProgram)(program);
                return Err(ShaderError(log));
            }

            let mut vao = 0;
            (epoxy::epoxy_glGenVertexArrays)(1, &mut vao);
            (epoxy::epoxy_glBindVertexArray)(vao);

            let mut vbo = 0;
            (epoxy::epoxy_glGenBuffers)(1, &mut vbo);
            (epoxy::epoxy_glBindBuffer)(gl::ARRAY_BUFFER, vbo);
            (epoxy::epoxy_glBufferData)(
                gl::ARRAY_BUFFER,
                std::mem::size_of_val(&QUAD) as gl::types::GLsizeiptr,
                QUAD.as_ptr() as *const std::ffi::c_void,
                gl::STATIC_DRAW,
            );
            (epoxy::epoxy_glEnableVertexAttribArray)(POSITION_LOCATION);
            (epoxy::epoxy_glVertexAttribPointer)(
                POSITION_LOCATION,
                2,
                gl::FLOAT,
                gl::FALSE,
                0,
                std::ptr::null(),
            );
            (epoxy::epoxy_glBindVertexArray)(0);

            Ok(Self {
                program,
                vao,
                vbo,
                profile,
            })
        }
    }

    /// The profile this program was compiled for.
    pub fn profile(&self) -> GlProfile {
        self.profile
    }

    /// Draw one frame. `width`/`height` are the framebuffer's pixel size
    /// (i.e. already scaled for HiDPI by the caller). The animation clock
    /// travels inside the model (`model.elapsed_ms`).
    pub fn render(&self, model: &RibbonModel, palette: &RibbonPalette, width: i32, height: i32) {
        let uniforms = pack_ribbon_uniforms(width as f64, height as f64, model, palette);

        unsafe {
            (epoxy::epoxy_glViewport)(0, 0, width, height);
            // The HUD is an overlay: start fully transparent and let the
            // shader's premultiplied output composite over the desktop.
            (epoxy::epoxy_glClearColor)(0.0, 0.0, 0.0, 0.0);
            (epoxy::epoxy_glClear)(gl::COLOR_BUFFER_BIT);
            (epoxy::epoxy_glEnable)(gl::BLEND);
            (epoxy::epoxy_glBlendFunc)(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

            (epoxy::epoxy_glUseProgram)(self.program);
            for (name, values) in &uniforms {
                let Ok(cname) = CString::new(name.as_str()) else {
                    continue;
                };
                let location = (epoxy::epoxy_glGetUniformLocation)(self.program, cname.as_ptr());
                if location < 0 {
                    // Dropped by the driver as unused — not an error.
                    continue;
                }
                match values.len() {
                    1 => (epoxy::epoxy_glUniform1fv)(location, 1, values.as_ptr()),
                    2 => (epoxy::epoxy_glUniform2fv)(location, 1, values.as_ptr()),
                    3 => (epoxy::epoxy_glUniform3fv)(location, 1, values.as_ptr()),
                    4 => (epoxy::epoxy_glUniform4fv)(location, 1, values.as_ptr()),
                    _ => continue,
                }
            }

            (epoxy::epoxy_glBindVertexArray)(self.vao);
            (epoxy::epoxy_glDrawArrays)(gl::TRIANGLES, 0, 6);
            (epoxy::epoxy_glBindVertexArray)(0);
            (epoxy::epoxy_glUseProgram)(0);
            (epoxy::epoxy_glDisable)(gl::BLEND);
        }
    }

    /// Release the GL objects. Call from `GLArea::connect_unrealize` while
    /// the context is still current.
    pub fn unrealize(&mut self) {
        unsafe {
            if self.vbo != 0 {
                (epoxy::epoxy_glDeleteBuffers)(1, &self.vbo);
                self.vbo = 0;
            }
            if self.vao != 0 {
                (epoxy::epoxy_glDeleteVertexArrays)(1, &self.vao);
                self.vao = 0;
            }
            if self.program != 0 {
                (epoxy::epoxy_glDeleteProgram)(self.program);
                self.program = 0;
            }
        }
    }
}

unsafe fn compile(kind: gl::types::GLenum, source: &str) -> Result<gl::types::GLuint, ShaderError> {
    let shader = (epoxy::epoxy_glCreateShader)(kind);
    let src = CString::new(source).map_err(|e| ShaderError(e.to_string()))?;
    let ptr: *const c_char = src.as_ptr();
    let len: gl::types::GLint = source.len() as _;
    (epoxy::epoxy_glShaderSource)(shader, 1, &ptr, &len);
    (epoxy::epoxy_glCompileShader)(shader);

    let mut status: gl::types::GLint = 0;
    (epoxy::epoxy_glGetShaderiv)(shader, gl::COMPILE_STATUS, &mut status);
    if status == 0 {
        let mut length: gl::types::GLint = 0;
        (epoxy::epoxy_glGetShaderiv)(shader, gl::INFO_LOG_LENGTH, &mut length);
        let mut buffer = vec![0u8; length.max(1) as usize];
        (epoxy::epoxy_glGetShaderInfoLog)(
            shader,
            buffer.len() as _,
            std::ptr::null_mut(),
            buffer.as_mut_ptr() as *mut c_char,
        );
        (epoxy::epoxy_glDeleteShader)(shader);
        let log = String::from_utf8_lossy(&buffer)
            .trim_end_matches('\0')
            .to_string();
        return Err(ShaderError(log));
    }
    Ok(shader)
}

unsafe fn program_log(program: gl::types::GLuint) -> String {
    let mut length: gl::types::GLint = 0;
    (epoxy::epoxy_glGetProgramiv)(program, gl::INFO_LOG_LENGTH, &mut length);
    let mut buffer = vec![0u8; length.max(1) as usize];
    (epoxy::epoxy_glGetProgramInfoLog)(
        program,
        buffer.len() as _,
        std::ptr::null_mut(),
        buffer.as_mut_ptr() as *mut c_char,
    );
    String::from_utf8_lossy(&buffer)
        .trim_end_matches('\0')
        .to_string()
}
