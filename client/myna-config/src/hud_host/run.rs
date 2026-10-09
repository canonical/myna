//! The glue: a GLib main loop, the bus name watch, the HUD subprocess and the
//! timers, driving [`Machine`].

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gio::glib::{self, ControlFlow};

use super::machine::{Action, Event, Machine, Note, EXIT_CANNOT_HOST};
use super::{launch, DAEMON_BUS_NAME};

struct Supervisor {
    machine: Machine,
    argv: Vec<String>,
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
    let argv = match launch::argv(|name| std::env::var(name).ok(), is_executable) {
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
        move |_, _, _| {
            if !owned.borrow().quitting {
                feed(&owned, Event::NameOwned);
            }
        },
        move |_, _| feed(&lost, Event::NameLost),
    );

    for signal in [libc::SIGTERM, libc::SIGINT] {
        let shared = shared.clone();
        glib::unix_signal_add_local(signal, move || {
            shared.borrow_mut().quitting = true;
            feed(&shared, Event::NameLost);
            ControlFlow::Break
        });
    }
    main_loop.run();
    0
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
