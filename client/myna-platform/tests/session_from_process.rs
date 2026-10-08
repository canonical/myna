//! `SessionEnv::from_process` against a real environment. Its own test
//! binary with one test, so setting variables races nothing.

use myna_platform::{Profile, SessionEnv};

#[test]
fn the_process_environment_is_read_and_a_dead_wayland_socket_dropped() {
    let runtime = std::env::temp_dir().join(format!("myna-platform-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &runtime);
    std::env::set_var("XDG_CURRENT_DESKTOP", "XFCE");
    std::env::set_var("XDG_SESSION_TYPE", "x11");
    std::env::set_var("WAYLAND_DISPLAY", "wayland-gone");
    std::env::set_var("DISPLAY", ":1");
    std::env::remove_var("MYNA_PLATFORM");

    let env = SessionEnv::from_process();
    assert_eq!(env.current_desktop.as_deref(), Some("XFCE"));
    assert_eq!(env.display.as_deref(), Some(":1"));
    assert_eq!(env.wayland_display, None);
    assert_eq!(Profile::select(&env), Ok(Profile::Xfce));

    std::fs::write(runtime.join("wayland-gone"), b"").unwrap();
    let env = SessionEnv::from_process();
    assert_eq!(env.wayland_display.as_deref(), Some("wayland-gone"));
    assert_eq!(Profile::select(&env), Ok(Profile::Generic));
    std::fs::remove_dir_all(&runtime).unwrap();
}
