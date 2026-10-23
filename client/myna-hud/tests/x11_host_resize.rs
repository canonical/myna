// tests/x11_host_resize.rs - `myna-hud --host x11` never lays its window
// out below the content's minimum while the pill changes size.
//
// GDK counts its own resize requests and lays the window out at whatever
// size the next ConfigureNotify carries, which need not be the reply to its
// resize. A window manager answers every configure request, a move included
// (ICCCM 4.1.5), so a move sent by the host while a resize is in flight can
// hand GDK the old size: "Gtk-CRITICAL: Allocation height too small", seen
// on Xubuntu. This window manager answers as xfwm4 does, a little late as
// xfwm4 is under load, and the HUD is driven through every size change a
// dictation makes. Gated like the other GTK suites (`ui-check`).

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureNotifyEvent, ConfigureWindowAux,
    ConnectionExt as _, EventMask, MapState, Window, CONFIGURE_NOTIFY_EVENT,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use myna_platform::status_surface::{placement, Point, Rect, Size, BOTTOM_MARGIN};
use support::{dictation, Headless, Hud, PrivateBus};

/// How late the window manager answers each request.
const LAG: Duration = Duration::from_millis(3);
const CYCLES: usize = 25;
const WAIT: Duration = Duration::from_secs(20);

/// A window manager that grants every request late, as xfwm4 does: a managed
/// window's size held within its `WM_NORMAL_HINTS` limits, and every request
/// for it, and its map, answered with a synthetic ConfigureNotify of its
/// geometry then.
fn window_manager(display: &str) {
    let (conn, screen) = x11rb::connect(Some(display)).expect("wm connection");
    let root = conn.setup().roots[screen].root;
    conn.change_window_attributes(
        root,
        &ChangeWindowAttributesAux::new()
            .event_mask(EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY),
    )
    .unwrap()
    .check()
    .expect("become the window manager");
    std::thread::spawn(move || {
        let mut managed: Vec<Window> = Vec::new();
        loop {
            let Ok(event) = conn.wait_for_event() else {
                return;
            };
            match event {
                Event::ConfigureRequest(e) => {
                    std::thread::sleep(LAG);
                    let mut request = ConfigureWindowAux::from_configure_request(&e);
                    if managed.contains(&e.window) {
                        let limits = size_limits(&conn, e.window);
                        request.width = request.width.map(|w| limits.0.clamp(w));
                        request.height = request.height.map(|h| limits.1.clamp(h));
                    }
                    let _ = conn.configure_window(e.window, &request);
                    if managed.contains(&e.window) {
                        let _ = synthetic_configure(&conn, root, e.window);
                    }
                    let _ = conn.flush();
                }
                Event::MapRequest(e) => {
                    std::thread::sleep(LAG);
                    if !managed.contains(&e.window) {
                        managed.push(e.window);
                    }
                    let _ = conn.map_window(e.window);
                    let _ = synthetic_configure(&conn, root, e.window);
                    let _ = conn.flush();
                }
                Event::UnmapNotify(e) => managed.retain(|&w| w != e.window),
                _ => {}
            }
        }
    });
}

/// A window's `WM_NORMAL_HINTS` width and height limits.
fn size_limits(conn: &RustConnection, window: Window) -> (Limit, Limit) {
    let hints: Vec<u32> = conn
        .get_property(
            false,
            window,
            AtomEnum::WM_NORMAL_HINTS,
            AtomEnum::ANY,
            0,
            18,
        )
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(Iterator::collect))
        .unwrap_or_default();
    let field = |flag: u32, index: usize| {
        (hints.len() > index + 1 && hints[0] & flag != 0).then(|| (hints[index], hints[index + 1]))
    };
    let (min, max) = (field(16, 5), field(32, 7));
    (
        Limit(min.map(|m| m.0), max.map(|m| m.0)),
        Limit(min.map(|m| m.1), max.map(|m| m.1)),
    )
}

struct Limit(Option<u32>, Option<u32>);

impl Limit {
    fn clamp(&self, value: u32) -> u32 {
        let value = self.1.map_or(value, |max| value.min(max));
        self.0.map_or(value, |min| value.max(min))
    }
}

fn synthetic_configure(conn: &RustConnection, root: Window, window: Window) -> Option<()> {
    let geometry = conn.get_geometry(window).ok()?.reply().ok()?;
    let at = conn
        .translate_coordinates(window, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    let event = ConfigureNotifyEvent {
        response_type: CONFIGURE_NOTIFY_EVENT,
        sequence: 0,
        event: window,
        window,
        above_sibling: x11rb::NONE,
        x: at.dst_x,
        y: at.dst_y,
        width: geometry.width,
        height: geometry.height,
        border_width: 0,
        override_redirect: false,
    };
    conn.send_event(false, window, EventMask::STRUCTURE_NOTIFY, event)
        .ok()?;
    Some(())
}

fn hud_window(conn: &RustConnection, root: Window) -> Option<Window> {
    let tree = conn.query_tree(root).ok()?.reply().ok()?;
    tree.children.into_iter().find(|&w| {
        conn.get_window_attributes(w)
            .ok()
            .and_then(|c| c.reply().ok())
            .is_some_and(|a| a.map_state == MapState::VIEWABLE)
    })
}

fn centred(conn: &RustConnection, root: Window, window: Window) -> bool {
    let (Some(geometry), Some(at)) = (
        conn.get_geometry(window).ok().and_then(|c| c.reply().ok()),
        conn.translate_coordinates(window, root, 0, 0)
            .ok()
            .and_then(|c| c.reply().ok()),
    ) else {
        return false;
    };
    let size = Size {
        width: geometry.width.into(),
        height: geometry.height.into(),
    };
    let screen = Rect {
        x: 0,
        y: 0,
        width: 1024,
        height: 768,
    };
    Point {
        x: at.dst_x.into(),
        y: at.dst_y.into(),
    } == placement(screen, size, BOTTOM_MARGIN)
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_pill_is_never_laid_out_under_its_content() {
    if std::env::var_os("MYNA_HUD_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_HUD_GTK_TESTS=1");
        return;
    }
    let bus = PrivateBus::spawn().expect("dbus-daemon");
    let headless = Headless::spawn("1024x768x24").expect("Xvfb");
    let (conn, screen) = x11rb::connect(Some(&headless.display)).expect("connect to Xvfb");
    let root = conn.setup().roots[screen].root;
    window_manager(&headless.display);

    let publisher = dictation::serve(&bus.address, "idle");

    let mut hud = Hud(Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host", "x11"])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("DISPLAY", &headless.display)
        .env("GIO_USE_VFS", "local")
        .env("GSETTINGS_BACKEND", "memory")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("GDK_SCALE")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn myna-hud"));
    let criticals = Arc::new(AtomicUsize::new(0));
    let log = Arc::new(Mutex::new(Vec::new()));
    {
        let stderr = hud.0.stderr.take().unwrap();
        let (criticals, log) = (criticals.clone(), log.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("CRITICAL") {
                    criticals.fetch_add(1, Ordering::SeqCst);
                }
                log.lock().unwrap().push(line);
            }
        });
    }

    // Every size change a dictation makes, mapped and across a remap: the
    // one-line pill, a headline that widens it and adds a line, and back.
    const LONG: &str = "Password field skipped (focused field is secure)";
    let steps: [(&str, &str); 8] = [
        ("recording", "Listening"),
        ("idle", ""),
        ("error", LONG),
        ("idle", ""),
        ("notice", "Focus lost"),
        ("error", LONG),
        ("recording", "Listening"),
        ("idle", ""),
    ];
    let mut failed = 0;
    for _ in 0..CYCLES {
        let before = criticals.load(Ordering::SeqCst);
        for (state, message) in steps {
            dictation::publish(&publisher, state, message);
            if state == "idle" {
                wait_until("idle unmaps the HUD", || hud_window(&conn, root).is_none());
            } else {
                wait_until("the HUD is mapped", || hud_window(&conn, root).is_some());
                std::thread::sleep(LAG * 4);
                wait_until("the HUD sits bottom-centre for its size", || {
                    hud_window(&conn, root).is_some_and(|w| centred(&conn, root, w))
                });
            }
        }
        failed += usize::from(criticals.load(Ordering::SeqCst) > before);
    }

    assert!(hud.0.try_wait().unwrap().is_none(), "myna-hud exited");
    let log = log.lock().unwrap().join("\n");
    assert_eq!(
        failed, 0,
        "{failed} of {CYCLES} cycles laid the pill out too small:\n{log}"
    );
}
