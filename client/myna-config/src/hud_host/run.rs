//! The glue: a GLib main loop, the bus name watch, the HUD subprocess and the
//! timers, driving [`Machine`].

use std::cell::RefCell;
use std::io::Read;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gio::glib::{self, ControlFlow};
use myna_platform::session::wayland_socket;
use myna_platform::{Session, SessionEnv, SessionKind};

use super::machine::{Action, Event, Machine, Note, EXIT_CANNOT_HOST};
use super::{launch, DAEMON_BUS_NAME, DAEMON_OBJECT_PATH};

struct Supervisor {
    machine: Machine,
    argv: Vec<String>,
    /// The session bus, as of the daemon's name last appearing.
    conn: Option<gio::DBusConnection>,
    child: Option<gio::Subprocess>,
    restart: Option<glib::SourceId>,
    kill: Option<glib::SourceId>,
    started: std::time::Instant,
    quitting: bool,
    main_loop: glib::MainLoop,
}

type Shared = Rc<RefCell<Supervisor>>;

fn log(message: &str) {
    eprintln!("myna-hud-host: {message}");
}

fn now_ms(sup: &Supervisor) -> u64 {
    sup.started.elapsed().as_millis() as u64
}

fn feed(shared: &Shared, event: Event) {
    let actions = shared.borrow_mut().machine.step(event);
    for action in actions {
        perform(shared, action);
    }
    let sup = shared.borrow();
    if sup.quitting && !sup.machine.has_child() {
        sup.main_loop.quit();
    }
}

fn perform(shared: &Shared, action: Action) {
    match action {
        Action::Register => tell_daemon(shared, "RegisterClient"),
        Action::Unregister => tell_daemon(shared, "UnregisterClient"),
        Action::Spawn => spawn(shared),
        Action::Terminate => {
            if let Some(child) = &shared.borrow().child {
                child.send_signal(libc::SIGTERM);
            }
        }
        Action::Kill => {
            if let Some(child) = &shared.borrow().child {
                child.force_exit();
            }
        }
        Action::ArmRestart(ms) => {
            let weak = Rc::downgrade(shared);
            let id = glib::timeout_add_local_once(Duration::from_millis(ms), move || {
                if let Some(shared) = weak.upgrade() {
                    shared.borrow_mut().restart = None;
                    feed(&shared, Event::RestartDue);
                }
            });
            shared.borrow_mut().restart = Some(id);
        }
        Action::CancelRestart => {
            if let Some(id) = shared.borrow_mut().restart.take() {
                id.remove();
            }
        }
        Action::ArmKill(ms) => {
            let weak = Rc::downgrade(shared);
            let id = glib::timeout_add_local_once(Duration::from_millis(ms), move || {
                if let Some(shared) = weak.upgrade() {
                    shared.borrow_mut().kill = None;
                    feed(&shared, Event::KillDue);
                }
            });
            shared.borrow_mut().kill = Some(id);
        }
        Action::CancelKill => {
            if let Some(id) = shared.borrow_mut().kill.take() {
                id.remove();
            }
        }
        Action::Log(Note::CannotHost) => log(&format!(
            "the HUD exited {EXIT_CANNOT_HOST}: this session cannot host it; not starting it again"
        )),
        Action::Log(Note::Dormant) => {
            log("the HUD keeps failing; giving up until the daemon restarts")
        }
    }
}

/// Say it to the daemon and wait for its answer: the registration must be on
/// the daemon's books before the HUD starts, and the host may be exiting. The
/// daemon has just appeared (or we are leaving), so the call returns at once;
/// the limit only bounds a wedged daemon, and a failure costs nothing but the
/// early registration, since the HUD registers itself in the end.
fn tell_daemon(shared: &Shared, method: &str) {
    let Some(conn) = shared.borrow().conn.clone() else {
        return;
    };
    let result = conn.call_sync(
        Some(DAEMON_BUS_NAME),
        DAEMON_OBJECT_PATH,
        DAEMON_BUS_NAME,
        method,
        None,
        None,
        gio::DBusCallFlags::NONE,
        2000,
        None::<&gio::Cancellable>,
    );
    if let Err(error) = result {
        log(&format!("{method} failed: {error}"));
    }
}

fn spawn(shared: &Shared) {
    let argv = shared.borrow().argv.clone();
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    match gio::Subprocess::newv(&args, gio::SubprocessFlags::NONE) {
        Ok(child) => {
            {
                let mut sup = shared.borrow_mut();
                sup.child = Some(child.clone());
            }
            let now = now_ms(&shared.borrow());
            feed(shared, Event::Spawned { now_ms: now });
            let weak = Rc::downgrade(shared);
            child.wait_async(None::<&gio::Cancellable>, move |_| {
                let Some(shared) = weak.upgrade() else { return };
                let code = {
                    let mut sup = shared.borrow_mut();
                    let code = sup
                        .child
                        .take()
                        .filter(|child| child.has_exited())
                        .map(|child| child.exit_status());
                    (code, now_ms(&sup))
                };
                feed(
                    &shared,
                    Event::Exited {
                        code: code.0,
                        now_ms: code.1,
                    },
                );
            });
        }
        Err(error) => {
            log(&format!("cannot start {}: {error}", argv[0]));
            let now = now_ms(&shared.borrow());
            feed(shared, Event::SpawnFailed { now_ms: now });
        }
    }
}

/// Supervise until SIGTERM/SIGINT; returns the process exit code.
pub fn run() -> i32 {
    let env = SessionEnv::from_process();
    let kind = Session::detect(&env).kind;
    let argv = match launch::argv(kind, |name| std::env::var(name).ok(), is_executable) {
        Ok(argv) => argv,
        Err(error) => {
            log(&format!("nothing to host: {error}"));
            return 0;
        }
    };
    let main_loop = glib::MainLoop::new(None, false);
    let shared: Shared = Rc::new(RefCell::new(Supervisor {
        machine: match std::env::var("MYNA_HUD_HOST_GRACE_MS")
            .ok()
            .and_then(|ms| ms.parse().ok())
        {
            Some(ms) => Machine::new().with_grace(ms),
            None => Machine::new(),
        },
        argv,
        conn: None,
        child: None,
        restart: None,
        kill: None,
        started: std::time::Instant::now(),
        quitting: false,
        main_loop: main_loop.clone(),
    }));

    let owned = shared.clone();
    let lost = shared.clone();
    let _watch = gio::bus_watch_name(
        gio::BusType::Session,
        DAEMON_BUS_NAME,
        gio::BusNameWatcherFlags::NONE,
        move |conn, _, _| {
            if !owned.borrow().quitting {
                owned.borrow_mut().conn = Some(conn);
                feed(&owned, Event::NameOwned);
            }
        },
        move |_, _| feed(&lost, Event::NameLost),
    );

    for signal in [libc::SIGTERM, libc::SIGINT] {
        let shared = shared.clone();
        glib::unix_signal_add_local(signal, move || {
            end(&shared);
            ControlFlow::Break
        });
    }
    watch_display(&shared, Display::connect(kind, &env));
    main_loop.run();
    0
}

/// Stop the HUD and leave.
fn end(shared: &Shared) {
    shared.borrow_mut().quitting = true;
    feed(shared, Event::Shutdown);
}

/// The session's display server, held only to learn when it goes.
enum Display {
    X11(Box<x11rb::rust_connection::RustConnection>),
    Wayland(UnixStream),
}

impl Display {
    /// The display the HUD will be hosted on; None when there is none.
    fn connect(kind: SessionKind, env: &SessionEnv) -> Option<Self> {
        match kind {
            SessionKind::Wayland => {
                let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
                let socket =
                    wayland_socket(env.wayland_display.as_deref()?, runtime_dir.as_deref())?;
                let stream = UnixStream::connect(socket).ok()?;
                stream.set_nonblocking(true).ok()?;
                Some(Self::Wayland(stream))
            }
            _ => x11rb::rust_connection::RustConnection::connect(None)
                .ok()
                .map(|(conn, _)| Self::X11(Box::new(conn))),
        }
    }

    fn fd(&self) -> RawFd {
        match self {
            Self::X11(conn) => conn.stream().as_raw_fd(),
            Self::Wayland(stream) => stream.as_raw_fd(),
        }
    }

    /// Whether what made the connection readable is its close. Nothing is
    /// asked of the server, so all it can say is goodbye.
    fn closed(&self) -> bool {
        use x11rb::connection::Connection;
        match self {
            Self::X11(conn) => conn.poll_for_event().is_err(),
            Self::Wayland(stream) => match (&mut &*stream).read(&mut [0; 64]) {
                Ok(0) => true,
                Ok(_) => false,
                Err(error) => error.kind() != std::io::ErrorKind::WouldBlock,
            },
        }
    }
}

/// End with the session's display. A logout leaves the lingering user
/// manager and its bus running, so the bus name never says the session is over
/// and the host would outlive it; the display going away does.
fn watch_display(shared: &Shared, display: Option<Display>) {
    let Some(display) = display else {
        return;
    };
    let shared = shared.clone();
    glib::source::unix_fd_add_local(
        display.fd(),
        glib::IOCondition::IN | glib::IOCondition::HUP | glib::IOCondition::ERR,
        move |_, condition| {
            if condition.intersects(glib::IOCondition::HUP | glib::IOCondition::ERR)
                || display.closed()
            {
                log("the display is gone; the session has ended");
                end(&shared);
                return ControlFlow::Break;
            }
            ControlFlow::Continue
        },
    );
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let candidates: Vec<std::path::PathBuf> = if path.contains('/') {
        vec![path.into()]
    } else {
        std::env::var_os("PATH")
            .map(|paths| {
                std::env::split_paths(&paths)
                    .map(|dir| dir.join(path))
                    .collect()
            })
            .unwrap_or_default()
    };
    candidates.iter().any(|candidate| {
        std::fs::metadata(candidate)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    })
}
