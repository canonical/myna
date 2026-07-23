//! `myna-desktop` — the push-to-talk dictation app (plan T21/T22).
//!
//! Composes activation, the IBus text injector, and an activity indicator over a
//! live `myna-audio` PipeWire capture source and the `myna-orchestrator` session
//! into a [`DesktopController`].
//!
//! ## Activation
//!
//! Dictation must inject into *another* app, so activation must not depend on
//! terminal focus. The default is **toggle-to-talk via a GNOME custom keyboard
//! shortcut**: `myna-desktop` runs as a background daemon listening on a control
//! socket, and a GNOME shortcut bound to `myna-desktop --toggle` pokes it (press
//! = start, press again = stop). This works for a plain unsandboxed binary on
//! GNOME/Wayland — no terminal focus, no portal, no app id. Run
//! `myna-desktop --install-shortcut` once to bind `Super+D` for you.
//!
//! Alternatives: `--portal` (GlobalShortcuts hold-to-talk — only works packaged
//! as a snap/flatpak, which GNOME grants an app identity); `--stdin` (terminal
//! debug — injects back into the terminal); `--overlay` (GTK activity overlay,
//! experimental: the window can steal focus on Wayland and cut the session).
//!
//! ```text
//!   myna-server --adapter whisper --socket /tmp/myna.sock &
//!   myna-desktop --install-shortcut                 # once: binds Super+D
//!   myna-desktop --socket /tmp/myna.sock --language en   # the daemon
//!   # focus a text field, tap Super+D, speak, tap Super+D → text is injected
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use myna_audio::{CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::dbus::serve::ZbusBus;
use myna_desktop::dbus::{DictationService, SharedBus};
use myna_desktop::indicator::dbus::{DbusIndicator, Readiness, ReadinessTee};
use myna_desktop::indicator::notify::NotifyIndicator;
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::shortcut::control::{default_socket_path, send_toggle, ControlTrigger};
use myna_desktop::shortcut::portal::GlobalShortcutTrigger;
use myna_desktop::{DesktopController, Indicator};
use myna_orchestrator::{run_dictation, OrchestratorEvent, StdinTrigger, StopHandle, WsUnixBackend};
use tokio::sync::mpsc;

const DEFAULT_SHORTCUT: &str = "SUPER+d";
const DEFAULT_ACCEL: &str = "<Super>d";

const USAGE: &str = "\
myna-desktop — push-to-talk dictation (T21/T22)

USAGE:
    myna-desktop --socket <path> [options]      # run the dictation daemon
    myna-desktop --toggle                       # start/stop the running daemon
    myna-desktop --install-shortcut [<accel>]   # bind a GNOME shortcut (Super+D)

Focus a text field, tap the shortcut to start, speak, tap again to stop. The
committed transcript is injected via IBus into that field.

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required for daemon)
    --language <lang>  language hint sent in the session config (e.g. en)
    --target <node>    PipeWire node.name to capture from (default: system default)
    --control <path>   control-socket path (default: $XDG_RUNTIME_DIR/myna-desktop.sock)
    --toggle           poke the running daemon (bind this to a GNOME shortcut)
    --install-shortcut bind a GNOME custom keybinding (accel, default <Super>d)
                       to `myna-desktop --toggle`, then exit
    --portal           activate via the GlobalShortcuts portal (packaged only)
    --stdin            DEBUG: drive from the terminal (injects into the terminal)
    --overlay          show the GTK activity overlay (experimental; may steal focus)
    --dbus             serve org.myna.Dictation on the session bus for the GNOME
                       Shell extension (falls back to notifications if no bus)
    -h, --help         show this help
";

#[derive(Default)]
struct Args {
    socket: Option<PathBuf>,
    language: Option<String>,
    target: Option<String>,
    control: Option<PathBuf>,
    shortcut: String,
    toggle: bool,
    install_shortcut: Option<String>,
    portal: bool,
    stdin: bool,
    overlay: bool,
    dbus: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args { shortcut: DEFAULT_SHORTCUT.to_string(), ..Default::default() };
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => a.socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--language" => a.language = Some(next(&mut it, "--language")?),
            "--target" => a.target = Some(next(&mut it, "--target")?),
            "--control" => a.control = Some(PathBuf::from(next(&mut it, "--control")?)),
            "--shortcut" => a.shortcut = next(&mut it, "--shortcut")?,
            "--toggle" => a.toggle = true,
            "--install-shortcut" => {
                // Optional accel argument (not starting with '-').
                let accel = match it.peek() {
                    Some(v) if !v.starts_with('-') => it.next(),
                    _ => None,
                };
                a.install_shortcut = Some(accel.unwrap_or_else(|| DEFAULT_ACCEL.to_string()));
            }
            "--portal" => a.portal = true,
            "--stdin" => a.stdin = true,
            "--overlay" => a.overlay = true,
            "--dbus" => a.dbus = true,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(a)
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn control_path(args: &Args) -> PathBuf {
    args.control.clone().unwrap_or_else(default_socket_path)
}

/// Build the per-Press session factory: a fresh backend connection + live
/// capture source, run through the orchestrator (capture-at-press, ready-gated).
///
/// NOTE (T21 follow-up): negotiate `input_format` from server capabilities
/// (capabilities.query) rather than assuming the default; the shipped adapters
/// accept the default 16 kHz s16le mono, so the MVP uses it directly.
fn make_session(
    args: &Args,
    readiness: Option<Readiness>,
    pump_bus: Option<SharedBus>,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send + 'static {
    let socket = args.socket.clone().expect("daemon requires --socket");
    let language = args.language.clone();
    let target = args.target.clone();
    move |events: mpsc::Sender<OrchestratorEvent>| {
        let backend = WsUnixBackend::new(&socket);
        let mut builder = CaptureSource::builder(AudioFormat::default());
        if let Some(node) = &target {
            builder = builder.target(node.clone());
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        let stop = source.stop_handle();
        let config = SessionConfig { language: language.clone(), ..Default::default() };
        // --dbus: pump the capture level meter onto org.myna.Dictation for the
        // session's lifetime (the pump ends when the source drops its stats
        // sender at session end). Grab the receiver before the source moves.
        let pump = pump_bus
            .clone()
            .map(|bus| (bus, source.stats()));
        let readiness = readiness.clone();
        let run: SessionRun = Box::pin(async move {
            if let Some((bus, stats)) = pump {
                tokio::spawn(myna_desktop::dbus::pump::run(bus, stats));
            }
            // Tee the event stream so the publisher can split loading/recording
            // (R4) when in --dbus mode; fresh cold readiness per session.
            match readiness {
                Some(r) => {
                    r.reset();
                    let mut sink = ReadinessTee::new(ChannelSink(events), r);
                    run_dictation(&backend, config, source, &mut sink).await
                }
                None => {
                    let mut sink = ChannelSink(events);
                    run_dictation(&backend, config, source, &mut sink).await
                }
            }
        });
        (run, stop)
    }
}

/// Build and run the controller with the given indicator (tokio side).
async fn run_controller(
    args: Args,
    indicator: impl Indicator + 'static,
    readiness: Option<Readiness>,
    pump_bus: Option<SharedBus>,
) -> ExitCode {
    let injector = match IbusInjector::connect().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot connect to IBus: {e}");
            eprintln!("  (is an IBus daemon running? try `ibus restart` / check `ibus address`)");
            return ExitCode::FAILURE;
        }
    };

    let builder = DesktopController::builder()
        .injector(injector)
        .indicator(indicator)
        .session(make_session(&args, readiness, pump_bus));

    let mut controller = if args.stdin {
        builder.trigger(StdinTrigger::new()).build()
    } else if args.portal {
        match GlobalShortcutTrigger::bind("dictate", Some(&args.shortcut)).await {
            Ok(trigger) => builder.trigger(trigger).build(),
            Err(e) => {
                eprintln!("cannot bind the GlobalShortcuts portal: {e}");
                eprintln!("  (the portal only serves packaged apps; drop --portal to use the");
                eprintln!("   control-socket + GNOME custom shortcut instead)");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match ControlTrigger::bind(control_path(&args)) {
            Ok(trigger) => builder.trigger(trigger).build(),
            Err(e) => {
                eprintln!("cannot bind control socket {}: {e}", control_path(&args).display());
                return ExitCode::FAILURE;
            }
        }
    };

    controller.run().await;
    ExitCode::SUCCESS
}

fn banner(args: &Args) {
    let sock = args.socket.as_ref().map(|s| s.display().to_string()).unwrap_or_default();
    if args.stdin {
        println!("myna-desktop → {sock} — DEBUG stdin: Enter to start/stop (injects into THIS terminal)");
    } else if args.portal {
        println!("myna-desktop → {sock} — hold {} to talk (portal)", args.shortcut);
    } else {
        println!("myna-desktop → {sock} — daemon ready; tap your dictation shortcut to start/stop.");
        println!("  if you haven't bound one yet: `myna-desktop --install-shortcut` (binds Super+D)");
        println!("  or bind a GNOME custom shortcut to: `{}`", toggle_command());
    }
}

fn exe_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "myna-desktop".to_string())
}

/// The command a desktop shortcut should invoke to poke the daemon. Under
/// snap confinement `current_exe` is a *revisioned* path that goes stale on
/// refresh — use the stable `/snap/bin/<instance>.toggle` entry instead.
fn toggle_command() -> String {
    if let Ok(instance) = std::env::var("SNAP_INSTANCE_NAME") {
        if !instance.is_empty() {
            return format!("/snap/bin/{instance}.toggle");
        }
    }
    format!("{} --toggle", exe_path())
}

/// `--install-shortcut`: bind a GNOME custom keybinding to `myna-desktop
/// --toggle`, appending to any existing custom keybindings (never clobbering).
fn install_shortcut(accel: &str) -> ExitCode {
    use std::process::Command;
    const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/";
    let kb_schema = format!("{SCHEMA}.custom-keybinding:{PATH}");
    let command = toggle_command();

    let gset = |args: &[&str]| Command::new("gsettings").args(args).status().map(|s| s.success()).unwrap_or(false);

    // Read the current list and append our path if absent (don't clobber).
    let current = Command::new("gsettings")
        .args(["get", SCHEMA, "custom-keybindings"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    let current = current.trim();
    let want = format!("'{PATH}'");
    let new_list = if current.contains(&want) {
        current.to_string()
    } else if current.is_empty() || current == "@as []" || current == "[]" {
        format!("['{PATH}']")
    } else {
        // insert after the opening '['
        current.replacen('[', &format!("[{want}, "), 1)
    };

    let ok = gset(&["set", SCHEMA, "custom-keybindings", &new_list])
        && gset(&["set", &kb_schema, "name", "myna dictation"])
        && gset(&["set", &kb_schema, "command", &command])
        && gset(&["set", &kb_schema, "binding", accel]);

    if ok {
        println!("bound {accel} → `{command}`");
        println!("start the daemon (`myna-desktop --socket <path>`), focus a field, tap {accel}.");
        ExitCode::SUCCESS
    } else {
        eprintln!("failed to set the GNOME keybinding via gsettings.");
        eprintln!("bind one manually: Settings → Keyboard → Custom Shortcuts → `{command}`");
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // Non-daemon subcommands first (no IBus / server needed).
    if let Some(accel) = &args.install_shortcut {
        return install_shortcut(accel);
    }
    if args.toggle {
        let path = control_path(&args);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        return match rt.block_on(send_toggle(&path)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("cannot reach the myna-desktop daemon at {}: {e}", path.display());
                eprintln!("  (is `myna-desktop --socket <path>` running?)");
                ExitCode::FAILURE
            }
        };
    }

    if args.socket.is_none() {
        eprintln!("--socket is required to run the daemon\n\n{USAGE}");
        return ExitCode::FAILURE;
    }
    banner(&args);

    // The GTK overlay (opt-in) needs the GLib main loop on the process main
    // thread; everything else runs headless with desktop notifications.
    #[cfg(feature = "ui-gtk")]
    if args.overlay {
        return run_with_overlay(args);
    }
    #[cfg(not(feature = "ui-gtk"))]
    if args.overlay {
        eprintln!("note: this build has no ui-gtk feature; using notifications");
    }

    run_headless(args)
}

/// Default path: a plain tokio runtime + desktop-notification feedback (no
/// focus-perturbing window).
fn run_headless(args: Args) -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = if args.dbus {
        rt.block_on(run_headless_dbus(args))
    } else {
        rt.block_on(run_controller(args, NotifyIndicator::new(), None, None))
    };
    println!("bye");
    code
}

/// `--dbus`: serve `org.myna.Dictation` and use the D-Bus publisher as the
/// indicator (feature 004 — the GNOME Shell extension consumes it). Falls back
/// to desktop notifications when the session bus is unreachable or the name
/// can't be owned — dictation itself never hard-fails (P15).
async fn run_headless_dbus(args: Args) -> ExitCode {
    match ZbusBus::serve().await {
        Ok(bus) => {
            let readiness = Readiness::new();
            let service = DictationService::new(bus);
            let indicator = DbusIndicator::new(service.bus(), readiness.clone());
            let pump_bus = service.bus();
            eprintln!("serving org.myna.Dictation on the session bus");
            run_controller(args, indicator, Some(readiness), Some(pump_bus)).await
        }
        Err(e) => {
            eprintln!("cannot serve org.myna.Dictation ({e}); falling back to notifications");
            eprintln!("  (a 'GUID mismatch' means DBUS_SESSION_BUS_ADDRESS is stale — e.g. a tmux/screen");
            eprintln!("   server surviving logout; fix with: export DBUS_SESSION_BUS_ADDRESS=unix:path=$XDG_RUNTIME_DIR/bus)");
            run_controller(args, NotifyIndicator::new(), None, None).await
        }
    }
}

/// Opt-in GTK overlay path (R6): GTK owns the main thread + GLib loop; the
/// controller runs on a worker thread with a `GtkIndicator` bridged over an
/// `async-channel`. When the session loop ends the sender drops, closing the
/// channel, which quits the GTK app (see `run_indicator_app`).
#[cfg(feature = "ui-gtk")]
fn run_with_overlay(args: Args) -> ExitCode {
    use myna_desktop::indicator::gtk::{run_indicator_app, GtkIndicator};

    let (tx, rx) = async_channel::unbounded();
    let worker = std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("cannot start async runtime: {e}");
                return ExitCode::FAILURE;
            }
        };
        rt.block_on(run_controller(args, GtkIndicator::new(tx), None, None))
    });

    let _gtk_code = run_indicator_app(rx);
    let code = worker.join().unwrap_or(ExitCode::FAILURE);
    println!("bye");
    code
}
