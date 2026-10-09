// tests/x11_host_e2e.rs - `myna-hud --host x11` placing and protecting its
// own window, read back the way a window manager sees it.
//
// The real binary runs on an Xvfb of its own against a fake publisher on a
// private bus. The test is the window manager: it holds SubstructureRedirect
// on the root, so it sees every map and configure request and every EWMH
// client message, and it snapshots the window's properties at map request
// time, which is when xfwm4 decides focus and placement. A fake panel
// reserves the bottom of the screen with `_NET_WM_STRUT_PARTIAL`.
//
// Needs Xvfb and dbus-daemon, which the workshop has; it fails without them.

mod support;

use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConfigureWindowAux, ConnectionExt as _, CreateWindowAux,
    EventMask, MapState, PropMode, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use myna_hud::dbus_consumer::{BUS_NAME, OBJECT_PATH};
use myna_platform::status_surface::{placement, Point, Rect, Size, BOTTOM_MARGIN};
use support::{Headless, Hud, PrivateBus};

const SCREEN: Size = Size {
    width: 1024,
    height: 768,
};
const WAIT: Duration = Duration::from_secs(20);

/// The publisher the HUD consumes, reduced to what it reads.
struct Dictation {
    state: String,
}

#[zbus::interface(name = "com.canonical.Myna.Dictation")]
impl Dictation {
    #[zbus(property)]
    fn state(&self) -> String {
        self.state.clone()
    }
    #[zbus(property)]
    fn status_message(&self) -> String {
        String::new()
    }
    #[zbus(property)]
    fn audio_rms(&self) -> f64 {
        0.0
    }
    #[zbus(property)]
    fn audio_peak(&self) -> f64 {
        0.0
    }
    #[zbus(property)]
    fn hud_style(&self) -> String {
        "bar".into()
    }
    fn register_client(&self) -> u32 {
        1
    }
}

fn set_state(connection: &zbus::blocking::Connection, state: &str) {
    let iface = connection
        .object_server()
        .interface::<_, Dictation>(OBJECT_PATH)
        .expect("served interface");
    iface.get_mut().state = state.into();
    zbus::block_on(iface.get().state_changed(iface.signal_emitter())).expect("emit State");
}

x11rb::atom_manager! {
    Atoms: AtomsCookie {
        WM_PROTOCOLS,
        WM_TAKE_FOCUS,
        _NET_WM_PID,
        _NET_WM_WINDOW_TYPE,
        _NET_WM_WINDOW_TYPE_NOTIFICATION,
        _NET_WM_STATE,
        _NET_WM_STATE_STICKY,
        _NET_WM_STATE_SKIP_TASKBAR,
        _NET_WM_STATE_SKIP_PAGER,
        _NET_WM_USER_TIME,
        _NET_WM_USER_TIME_WINDOW,
        _NET_WORKAREA,
        _NET_CLIENT_LIST,
        _NET_WM_STRUT_PARTIAL,
        _NET_WM_WINDOW_TYPE_DOCK,
        _NET_SUPPORTED,
        _NET_SUPPORTING_WM_CHECK,
    }
}

/// What the window carried when it asked to be mapped.
#[derive(Debug)]
struct AtMap {
    window: Window,
    position: Point,
    size: Size,
    window_type: Vec<u32>,
    protocols: Vec<u32>,
    user_time: Vec<u32>,
}

#[derive(Debug)]
enum Seen {
    Map(AtMap),
    Message {
        window: Window,
        kind: u32,
        data: [u32; 5],
    },
    /// The window's input shape was set, after it was managed.
    InputShape(Window),
}

fn cardinals(conn: &RustConnection, window: Window, property: u32) -> Vec<u32> {
    conn.get_property(false, window, property, AtomEnum::ANY, 0, 1024)
        .ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| r.value32().map(Iterator::collect))
        .unwrap_or_default()
}

/// A window manager that maps and configures as asked and reports what it
/// saw. Runs until the process ends.
fn window_manager(display: &str, atoms: Atoms) -> Receiver<Seen> {
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
    // Announced as EWMH, as xfwm4 is: GDK puts the user time on its user
    // time window only for a manager that supports one.
    let check = conn.generate_id().unwrap();
    conn.create_window(
        0,
        check,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_ONLY,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    for window in [root, check] {
        conn.change_property32(
            PropMode::REPLACE,
            window,
            atoms._NET_SUPPORTING_WM_CHECK,
            AtomEnum::WINDOW,
            &[check],
        )
        .unwrap();
    }
    conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_SUPPORTED,
        AtomEnum::ATOM,
        &[
            atoms._NET_WM_USER_TIME_WINDOW,
            atoms._NET_WM_USER_TIME,
            atoms._NET_WM_STATE,
            atoms._NET_WM_WINDOW_TYPE,
        ],
    )
    .unwrap();
    conn.sync().unwrap();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || loop {
        let Ok(event) = conn.wait_for_event() else {
            return;
        };
        let seen = match event {
            Event::ConfigureRequest(e) => {
                let _ = conn
                    .configure_window(e.window, &ConfigureWindowAux::from_configure_request(&e));
                let _ = conn.flush();
                continue;
            }
            Event::MapRequest(e) => {
                let geometry = conn.get_geometry(e.window).unwrap().reply().unwrap();
                let user_time_window = cardinals(&conn, e.window, atoms._NET_WM_USER_TIME_WINDOW)
                    .first()
                    .copied()
                    .unwrap_or(e.window);
                let seen = Seen::Map(AtMap {
                    window: e.window,
                    position: Point {
                        x: geometry.x.into(),
                        y: geometry.y.into(),
                    },
                    size: Size {
                        width: geometry.width.into(),
                        height: geometry.height.into(),
                    },
                    window_type: cardinals(&conn, e.window, atoms._NET_WM_WINDOW_TYPE),
                    protocols: cardinals(&conn, e.window, atoms.WM_PROTOCOLS),
                    user_time: cardinals(&conn, user_time_window, atoms._NET_WM_USER_TIME),
                });
                // As xfwm4 does, place a window that names no position.
                let hints = cardinals(&conn, e.window, AtomEnum::WM_NORMAL_HINTS.into());
                if hints.first().is_none_or(|flags| flags & 5 == 0) {
                    let _ = conn.configure_window(e.window, &ConfigureWindowAux::new().x(0).y(0));
                }
                // As xfwm4 does when it frames a client: it copies the input
                // shape to its frame only on a ShapeNotify.
                let _ = conn.shape_select_input(e.window, true);
                let _ = conn.map_window(e.window);
                let _ = conn.flush();
                seen
            }
            Event::ShapeNotify(e) if e.shape_kind == SK::INPUT => {
                Seen::InputShape(e.affected_window)
            }
            Event::ClientMessage(e) => Seen::Message {
                window: e.window,
                kind: e.type_,
                data: e.data.as_data32(),
            },
            _ => continue,
        };
        if sender.send(seen).is_err() {
            return;
        }
    });
    receiver
}

fn next_map(seen: &Receiver<Seen>, messages: &mut Vec<Seen>) -> AtMap {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match seen.recv_timeout(left) {
            Ok(Seen::Map(at_map)) => return at_map,
            Ok(message) => messages.push(message),
            Err(e) => panic!("the HUD never asked to be mapped: {e}"),
        }
    }
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A panel window reserving `height` px along the bottom edge.
fn set_panel(conn: &RustConnection, atoms: &Atoms, root: Window, panel: Window, height: u32) {
    let strut = [
        0,
        0,
        0,
        height,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        SCREEN.width as u32 - 1,
    ];
    conn.change_property32(
        PropMode::REPLACE,
        panel,
        atoms._NET_WM_STRUT_PARTIAL,
        AtomEnum::CARDINAL,
        &strut,
    )
    .unwrap();
    // The window manager republishes the work area when struts change.
    let area = [0, 0, SCREEN.width as u32, SCREEN.height as u32 - height];
    conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_WORKAREA,
        AtomEnum::CARDINAL,
        &area,
    )
    .unwrap();
    conn.flush().unwrap();
}

/// Bottom-centre with `reserved` px kept clear at the bottom.
fn expected(size: Size, reserved: i32) -> Point {
    let area = Rect {
        x: 0,
        y: 0,
        width: SCREEN.width,
        height: SCREEN.height - reserved,
    };
    placement(area, size, BOTTOM_MARGIN)
}

fn root_position(conn: &RustConnection, root: Window, window: Window) -> Point {
    let at = conn
        .translate_coordinates(window, root, 0, 0)
        .unwrap()
        .reply()
        .unwrap();
    Point {
        x: at.dst_x.into(),
        y: at.dst_y.into(),
    }
}

#[test]
fn the_hud_hosts_itself_on_x11() {
    let bus = PrivateBus::spawn().expect("dbus-daemon (the workshop installs it)");
    let headless = Headless::spawn(&format!("{}x{}x24", SCREEN.width, SCREEN.height))
        .expect("Xvfb (the workshop installs it)");

    let (conn, screen) = x11rb::connect(Some(&headless.display)).expect("connect to Xvfb");
    let root = conn.setup().roots[screen].root;
    let atoms = Atoms::new(&conn).unwrap().reply().unwrap();

    let panel = conn.generate_id().unwrap();
    conn.create_window(
        0,
        panel,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    // A launcher dock with no strut, autohidden below the screen edge as
    // Xubuntu's is: 49 px reserved, more than the panel's 40.
    let dock = conn.generate_id().unwrap();
    conn.create_window(
        0,
        dock,
        root,
        362,
        SCREEN.height as i16 + 10,
        300,
        49,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    conn.change_property32(
        PropMode::REPLACE,
        dock,
        atoms._NET_WM_WINDOW_TYPE,
        AtomEnum::ATOM,
        &[atoms._NET_WM_WINDOW_TYPE_DOCK],
    )
    .unwrap();
    // Mapped before the window manager starts, so it is not asked.
    conn.map_window(dock).unwrap();
    conn.sync().unwrap();
    let seen = window_manager(&headless.display, atoms);
    conn.change_property32(
        PropMode::REPLACE,
        root,
        atoms._NET_CLIENT_LIST,
        AtomEnum::WINDOW,
        &[panel, dock],
    )
    .unwrap();
    set_panel(&conn, &atoms, root, panel, 40);

    let publisher = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(BUS_NAME)
        .unwrap()
        .serve_at(
            OBJECT_PATH,
            Dictation {
                state: "recording".into(),
            },
        )
        .unwrap()
        .build()
        .expect("publish com.canonical.Myna.Dictation");

    let mut hud = Hud(Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host", "x11"])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("DISPLAY", &headless.display)
        .env("GIO_USE_VFS", "local")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("GDK_SCALE")
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn myna-hud"));
    let pid = hud.0.id();

    // First map: everything a window manager decides on is already set.
    let mut messages = Vec::new();
    let first = next_map(&seen, &mut messages);
    assert_eq!(
        cardinals(&conn, first.window, atoms._NET_WM_PID),
        vec![pid],
        "the mapped window is not the HUD's"
    );
    assert_eq!(
        first.window_type,
        vec![atoms._NET_WM_WINDOW_TYPE_NOTIFICATION]
    );
    assert!(!first.protocols.is_empty(), "GDK's protocols were dropped");
    assert!(
        !first.protocols.contains(&atoms.WM_TAKE_FOCUS),
        "WM_TAKE_FOCUS makes the window focusable"
    );
    assert_eq!(
        first.user_time,
        vec![0],
        "a zero user time refuses focus on map"
    );
    let target = expected(first.size, 49);
    assert_eq!(first.position, target, "mapped away from bottom-centre");
    // Wherever the manager put it, it ends up there.
    wait_until("the mapped HUD is in place", || {
        root_position(&conn, root, first.window) == target
    });

    // After the map: input refused, sticky asked for, skip hints, no input.
    wait_until("WM_HINTS refuse input", || {
        let hints = cardinals(&conn, first.window, AtomEnum::WM_HINTS.into());
        hints.len() >= 2 && hints[0] & 1 == 1 && hints[1] == 0
    });
    let state = cardinals(&conn, first.window, atoms._NET_WM_STATE);
    assert!(
        state.contains(&atoms._NET_WM_STATE_SKIP_TASKBAR),
        "{state:?}"
    );
    assert!(state.contains(&atoms._NET_WM_STATE_SKIP_PAGER), "{state:?}");
    let deadline = Instant::now() + WAIT;
    while !messages.iter().any(|m| {
        matches!(m, Seen::Message { window, kind, data }
            if *window == first.window && *kind == atoms._NET_WM_STATE
                && data[0] == 1 && data[1] == atoms._NET_WM_STATE_STICKY)
    }) {
        let left = deadline.saturating_duration_since(Instant::now());
        messages.push(seen.recv_timeout(left).expect("no sticky request"));
    }
    // Reshaped once managed, or xfwm4's frame keeps taking the clicks.
    let deadline = Instant::now() + WAIT;
    while !messages
        .iter()
        .any(|m| matches!(m, Seen::InputShape(window) if *window == first.window))
    {
        let left = deadline.saturating_duration_since(Instant::now());
        messages.push(
            seen.recv_timeout(left)
                .expect("no input shape after the map"),
        );
    }
    let input = conn
        .shape_get_rectangles(first.window, SK::INPUT)
        .unwrap()
        .reply()
        .unwrap();
    assert!(input.rectangles.is_empty(), "the HUD takes pointer input");
    assert_eq!(
        conn.get_window_attributes(first.window)
            .unwrap()
            .reply()
            .unwrap()
            .map_state,
        MapState::VIEWABLE
    );

    // A taller panel while mapped: the HUD follows the work area.
    set_panel(&conn, &atoms, root, panel, 100);
    let target = expected(first.size, 100);
    wait_until("the HUD follows the new work area", || {
        root_position(&conn, root, first.window) == target
    });

    // Idle unmaps; the next map is placed before it happens, again.
    set_state(&publisher, "idle");
    wait_until("idle unmaps the HUD", || {
        conn.get_window_attributes(first.window)
            .unwrap()
            .reply()
            .unwrap()
            .map_state
            != MapState::VIEWABLE
    });
    set_panel(&conn, &atoms, root, panel, 60);
    set_state(&publisher, "recording");
    let second = next_map(&seen, &mut messages);
    assert_eq!(second.window, first.window);
    assert_eq!(second.position, expected(second.size, 60));

    // Stopped as its supervisor stops it, it leaves cleanly (and, under
    // coverage, writes its profile).
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = hud.0.try_wait().expect("try_wait") {
            break status;
        }
        assert!(Instant::now() < deadline, "myna-hud ignored SIGTERM");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(status.code(), Some(0), "myna-hud exited {status}");
}

#[test]
fn the_x11_host_refuses_a_wayland_session() {
    let runtime = std::env::temp_dir().join(format!("myna-hud-x11-host-{}", std::process::id()));
    std::fs::create_dir_all(&runtime).unwrap();
    let socket = runtime.join("wayland-test");
    let _ = std::fs::remove_file(&socket);
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host=x11"])
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("WAYLAND_DISPLAY", "wayland-test")
        .env("DISPLAY", ":99")
        .output()
        .expect("run myna-hud");
    let _ = std::fs::remove_dir_all(&runtime);

    assert_eq!(output.status.code(), Some(78), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("needs an X11 session"), "{stderr}");
}

#[test]
fn an_unknown_host_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_myna-hud"))
        .args(["--host", "mutter"])
        .output()
        .expect("run myna-hud");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown host mutter"));
}
