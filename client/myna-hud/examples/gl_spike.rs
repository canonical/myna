// examples/gl_spike.rs — T101: a throwaway de-risking spike for the GLArea
// GPU path (R23). NOT shipped product code: it exists to prove, under
// `xvfb-run`, that
//
//   1. a Gtk.GLArea gets a working GL context in this environment,
//   2. raw GL calls work through libepoxy's per-function dispatch tables —
//      the loader GTK itself uses (epoxy exports `epoxy_<fn>` as function
//      POINTER variables, exactly what its C headers dereference; Ubuntu's
//      libepoxy has no generic `epoxy_get_proc_address`, so the `gl` crate's
//      load_with loader is the wrong tool — this spike's discovery, recorded
//      here for T121), and
//   3. the REAL generated ribbon shader (shader::build_ribbon_shader,
//      wrapped for the context's profile) compiles on the actual driver.
//
// Run with:   xvfb-run -a cargo run -p myna-hud --example gl_spike
// Exit code 0 = all three hold.

use std::ffi::{CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::glib;
use gtk::prelude::*;
use gtk4 as gtk;

// The gl crate supplies types + constants only here; functions come from
// epoxy's dispatch tables (per-function, auto-selected for the current
// context's API — desktop GL or GLES, which is exactly our dual-profile
// story).
#[allow(non_snake_case)]
mod epoxy {
    use gl::types::*;

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
        fn epoxy_glShaderSource(shader: GLuint, count: GLsizei, string: *const *const std::os::raw::c_char, length: *const GLint);
        fn epoxy_glCompileShader(shader: GLuint);
        fn epoxy_glGetShaderiv(shader: GLuint, pname: GLenum, params: *mut GLint);
        fn epoxy_glGetShaderInfoLog(shader: GLuint, bufSize: GLsizei, length: *mut GLsizei, infoLog: *mut std::os::raw::c_char);
        fn epoxy_glDeleteShader(shader: GLuint);
        fn epoxy_glGetString(name: GLenum) -> *const GLubyte;
        fn epoxy_glClearColor(red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat);
        fn epoxy_glClear(mask: GLbitfield);
    }
}

fn compile_fragment(source: &str) -> Result<(), String> {
    unsafe {
        let shader = (epoxy::epoxy_glCreateShader)(gl::FRAGMENT_SHADER);
        let src = CString::new(source).unwrap();
        let src_ptr: *const std::os::raw::c_char = src.as_ptr();
        let len: gl::types::GLint = source.len() as _;
        (epoxy::epoxy_glShaderSource)(shader, 1, &src_ptr, &len);
        (epoxy::epoxy_glCompileShader)(shader);
        let mut status = 0i32;
        (epoxy::epoxy_glGetShaderiv)(shader, gl::COMPILE_STATUS, &mut status);
        let mut log_len = 0i32;
        (epoxy::epoxy_glGetShaderiv)(shader, gl::INFO_LOG_LENGTH, &mut log_len);
        let mut log = if log_len > 0 {
            vec![0u8; log_len as usize]
        } else {
            Vec::new()
        };
        if !log.is_empty() {
            (epoxy::epoxy_glGetShaderInfoLog)(
                shader,
                log_len,
                std::ptr::null_mut(),
                log.as_mut_ptr().cast(),
            );
        }
        (epoxy::epoxy_glDeleteShader)(shader);
        let log = String::from_utf8_lossy(&log).trim().to_string();
        if status == 0 {
            Err(log)
        } else {
            Ok(())
        }
    }
}

fn main() {
    let app = gtk::Application::new(None::<&str>, gtk::gio::ApplicationFlags::empty());
    app.connect_activate(|app| {
        let app = app.clone();
        let render_app = app.clone();
        let area = gtk::GLArea::new();
        area.set_size_request(160, 32);

        let compiled = std::rc::Rc::new(AtomicBool::new(false));
        let compiled_render = compiled.clone();
        area.connect_render(move |_area, _ctx| {
            let app = render_app.clone();

            unsafe {
                let version_ptr = (epoxy::epoxy_glGetString)(gl::VERSION);
                let version = if version_ptr.is_null() {
                    "<null>".to_string()
                } else {
                    CStr::from_ptr(version_ptr.cast())
                        .to_string_lossy()
                        .into_owned()
                };
                println!("gl-spike: GL_VERSION = {version}");
            }

            let source = myna_hud::shader::build_ribbon_shader();
            // Try the ES 3.00 profile first (the production profile on
            // Wayland/GLES), then desktop GL 1.20 — either compiling on the
            // real driver proves the generator.
            let es300 =
                myna_hud::shader::standalone_shader(&source, myna_hud::shader::GlProfile::Es300);
            let gl120 =
                myna_hud::shader::standalone_shader(&source, myna_hud::shader::GlProfile::Gl120);
            match compile_fragment(&es300).or_else(|_| compile_fragment(&gl120)) {
                Ok(()) => {
                    println!("gl-spike: ribbon shader compiled on the real driver");
                    compiled_render.store(true, Ordering::SeqCst);
                }
                Err(log) => eprintln!("gl-spike: shader compile FAILED: {log}"),
            }

            unsafe {
                (epoxy::epoxy_glClearColor)(0.08, 0.16, 0.24, 1.0);
                (epoxy::epoxy_glClear)(gl::COLOR_BUFFER_BIT);
            }

            glib::timeout_add_local_once(std::time::Duration::from_millis(200), {
                let compiled = compiled.clone();
                move || {
                    let code = if compiled.load(Ordering::SeqCst) {
                        0
                    } else {
                        1
                    };
                    app.quit();
                    std::process::exit(code);
                }
            });
            glib::Propagation::Proceed
        });

        let win = gtk::ApplicationWindow::new(&app);
        win.set_title(Some("myna gl-spike"));
        win.set_child(Some(&area));
        win.present();
    });
    std::process::exit(app.run().get() as i32);
}
