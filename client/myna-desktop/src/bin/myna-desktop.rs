//! `myna-desktop` — the push-to-talk dictation app (plan T21/T22).
//!
//! Composes activation, the IBus text injector, and an activity indicator over a
//! live `myna-audio` PipeWire capture source and the `myna-orchestrator` session
//! into a [`DesktopController`].
//!
//! ## Activation
//!
//! Dictation must inject into *another* app, so activation must not depend on
//! terminal focus. Two transports, and **the daemon picks between them itself**
//! ([`Activation::resolve`]) — which one is correct is a property of how the
//! binary was packaged, not a preference a user can hold:
//!
//! - **GlobalShortcuts portal** — the sandboxed-native trigger, and the default
//!   whenever `$SNAP` is set. GNOME only grants a portal app identity to a
//!   packaged app, so this is available exactly when packaged.
//! - **Control socket** — the default unpackaged. `myna-desktop` listens on a
//!   control socket and a GNOME custom shortcut bound to `myna-desktop --toggle`
//!   pokes it. Run `myna-desktop --install-shortcut '<Super>t'` once to bind it.
//!
//! Both are press-to-toggle: tap to start, tap again to stop. `--portal` /
//! `--control` force one; `--hold` switches the portal to hold-to-talk;
//! `--stdin` is terminal debug (injects back into the terminal); `--overlay` is
//! the experimental GTK activity overlay (the window can steal focus on Wayland
//! and cut the session).
//!
//! ```text
//!   myna-server --adapter whisper --socket /tmp/myna.sock &
//!   myna-desktop --install-shortcut '<Super>t'      # once, unpackaged: bind a key
//!   myna-desktop --socket /tmp/myna.sock --language en   # the daemon
//!   # focus a text field, tap the shortcut, speak, tap again → text is injected
//! ```
//!
//! ## Things that are resolved, not asked
//!
//! Three switches used to be the user's problem and are now the daemon's:
//! the indicator bus (`org.myna.Dictation` is always served, falling back to
//! notifications by itself), the activation transport (above), and streaming
//! preedit ([`resolve_preedit`]). Each still has an explicit override for
//! debugging, but a correct setup requires none of them.

use std::path::PathBuf;
use std::process::ExitCode;

use myna_audio::{CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::dbus::serve::{ServeError, ZbusBus};
use myna_desktop::dbus::{DictationService, SharedBus};
use myna_desktop::indicator::dbus::{DbusIndicator, Readiness, ReadinessTee};
use myna_desktop::indicator::notify::NotifyIndicator;
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::shortcut::control::{default_socket_path, send_toggle, ControlTrigger};
use myna_desktop::shortcut::portal::{ActivationMode, GlobalShortcutTrigger};
use myna_desktop::{DesktopController, Indicator};
use myna_orchestrator::{
    run_dictation, OrchestratorEvent, StdinTrigger, StopHandle, WsUnixBackend,
};
use tokio::sync::mpsc;

const USAGE: &str = "\
myna-desktop — push-to-talk dictation (T21/T22)

USAGE:
    myna-desktop --socket <path> [options]      # run the dictation daemon
    myna-desktop --toggle                       # start/stop the running daemon
    myna-desktop --install-shortcut <accel>     # bind a GNOME shortcut (e.g. '<Super>t')

Focus a text field, tap the shortcut to start, speak, tap again to stop. The
committed transcript is injected via IBus into that field.

The daemon always serves org.myna.Dictation for the GNOME Shell extension,
picks its activation transport from how it was packaged, and decides streaming
preedit from your persisted mode preference. A correct setup needs none of the
overrides below.

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required for daemon)
    --language <lang>  language hint sent in the session config (e.g. en)
    --target <node>    PipeWire node.name to capture from (default: system default)
    --control-socket <path>
                       control-socket path (default: $XDG_RUNTIME_DIR/myna-desktop.sock)
    --toggle           poke the running daemon (bind this to a GNOME shortcut)
    --install-shortcut bind a GNOME custom keybinding to <accel>
                       (e.g. '<Super>t'), then exit
    --shortcut <accel> preferred trigger in portal activation (the portal's bind
                       dialog may still let you pick a different key)
    --hold             portal activation: hold-to-talk instead (hold = record)
    --overlay          show the GTK activity overlay (experimental; may steal focus)

ACTIVATION (default: portal when packaged — $SNAP set — else control socket):
    --portal           force the GlobalShortcuts portal (packaged builds only)
    --control          force the control socket (poke it with --toggle)
    --stdin            DEBUG: drive from the terminal (injects into the terminal)

OVERRIDES (for debugging; the daemon resolves all three by itself):
    --preedit          force the in-flight hypothesis into the field's preedit
    --no-preedit       force commit-only, even in streaming mode
    --no-dbus          do not serve org.myna.Dictation (notifications only).
                       Also opts out of the one-daemon-at-a-time guard, which
                       is that name: two daemons split the hotkey from the UI

    -h, --help         show this help
";

/// How a press reaches the daemon. Which one is correct follows from how the
/// binary was packaged, so [`Args::activation`] holds `None` ("resolve it")
/// unless the user forced one.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Activation {
    /// GlobalShortcuts portal — needs the app identity only a packaged build has.
    Portal,
    /// Control socket + a desktop custom shortcut bound to `--toggle`.
    Control,
    /// DEBUG: Enter on stdin; injects back into the launching terminal.
    Stdin,
}

impl Activation {
    /// The portal only serves apps the compositor can identify, which on GNOME
    /// means a packaged one — so `$SNAP` *is* the availability test, not a
    /// heuristic. Unpackaged builds get the control socket, which needs no app
    /// identity.
    fn resolve(forced: Option<Activation>) -> Activation {
        forced.unwrap_or({
            if std::env::var_os("SNAP").is_some() {
                Activation::Portal
            } else {
                Activation::Control
            }
        })
    }
}

#[derive(Debug, Default)]
struct Args {
    socket: Option<PathBuf>,
    language: Option<String>,
    target: Option<String>,
    control: Option<PathBuf>,
    shortcut: Option<String>,
    toggle: bool,
    install_shortcut: Option<String>,
    /// `None` = resolve from packaging; `Some` = the user forced one.
    activation: Option<Activation>,
    hold: bool,
    overlay: bool,
    /// `None` = resolve from the persisted streaming mode; `Some` = forced.
    preedit: Option<bool>,
    no_dbus: bool,
}

fn parse_args() -> Result<Args, String> {
    parse_args_from(std::env::args().skip(1).peekable())
}

fn parse_args_from(
    mut it: std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<Args, String> {
    let mut a = Args::default();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => a.socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--language" => a.language = Some(next(&mut it, "--language")?),
            "--target" => a.target = Some(next(&mut it, "--target")?),
            "--control-socket" => {
                a.control = Some(PathBuf::from(next(&mut it, "--control-socket")?))
            }
            "--shortcut" => a.shortcut = Some(next(&mut it, "--shortcut")?),
            "--toggle" => a.toggle = true,
            "--install-shortcut" => {
                a.install_shortcut = Some(next(&mut it, "--install-shortcut")?);
            }
            "--portal" => set_activation(&mut a, Activation::Portal)?,
            "--control" => set_activation(&mut a, Activation::Control)?,
            "--stdin" => set_activation(&mut a, Activation::Stdin)?,
            "--hold" => a.hold = true,
            "--overlay" => a.overlay = true,
            "--preedit" => a.preedit = Some(true),
            "--no-preedit" => a.preedit = Some(false),
            "--no-dbus" => a.no_dbus = true,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    // `--hold` is a portal concept (the portal reports press and release; the
    // control socket only ever delivers a single poke). Reject it against a
    // resolved non-portal transport rather than ignoring it, so "hold-to-talk
    // silently does nothing" is not a mode a user can end up in.
    if a.hold && Activation::resolve(a.activation) != Activation::Portal {
        return Err("--hold only applies to portal activation (add --portal)".into());
    }
    Ok(a)
}

/// The activation flags are mutually exclusive: silently letting the last one
/// win would make `--portal --stdin` look like it worked.
fn set_activation(a: &mut Args, mode: Activation) -> Result<(), String> {
    match a.activation {
        Some(existing) if existing != mode => Err(format!(
            "conflicting activation flags: {existing:?} and {mode:?} (pick one)"
        )),
        _ => {
            a.activation = Some(mode);
            Ok(())
        }
    }
}

/// Streaming preedit (R9) is a consequence of the transcription mode, not a
/// separate preference: hypotheses only exist in streaming mode, so "show
/// them in the field" is decided by the same tier gate that decides streaming
/// ([`myna_core::effective_mode`]) — no server round-trip needed, and a
/// batch-only backend simply never emits `Unstable` anyway.
///
/// The injector still has the final say downstream: the controller renders a
/// preedit only where the backend has a real preedit region
/// (`Injector::supports_preedit`).
fn resolve_preedit(forced: Option<bool>) -> bool {
    forced.unwrap_or_else(|| {
        myna_core::effective_mode(myna_core::Settings::load().streaming_mode)
            == myna_core::StreamingMode::Streaming
    })
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
        let config = SessionConfig {
            language: language.clone(),
            ..Default::default()
        };
        // --dbus: pump the capture level meter onto org.myna.Dictation for the
        // session's lifetime (the pump ends when the source drops its stats
        // sender at session end). Grab the receiver before the source moves.
        let pump = pump_bus.clone().map(|bus| (bus, source.stats()));
        let readiness = readiness.clone();
        // Reset readiness synchronously, right here — when the session is
        // created (i.e. when the controller calls `session.start()`), not
        // lazily inside the `async move` block below. The controller
        // publishes `IndicatorState::Recording` immediately after
        // `session.start()` returns, before this future is ever polled; if
        // `ready_seen` were still `true` from the *previous* utterance at
        // that moment, `map_state` would publish `"recording"` first, only
        // flipping to `"loading"` once this future is finally polled and the
        // old `r.reset()` ran — a spurious recording→loading→recording
        // triple-flip on the wire that the GNOME Shell HUD renders as a
        // flicker on (re)start (no debounce there, by design elsewhere).
        // Resetting here instead means `ready_seen` is already `false` before
        // the controller ever reads it, so only one clean loading→recording
        // transition is published.
        if let Some(r) = &readiness {
            r.reset();
        }
        let run: SessionRun = Box::pin(async move {
            if let Some((bus, stats)) = pump {
                tokio::spawn(myna_desktop::dbus::pump::run(bus, stats));
            }
            // Tee the event stream so the publisher can split loading/recording
            // (R4) when in --dbus mode.
            match readiness {
                Some(r) => {
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
        .session(make_session(&args, readiness, pump_bus))
        .preedit(resolve_preedit(args.preedit));

    let mut controller = match Activation::resolve(args.activation) {
        Activation::Stdin => builder.trigger(StdinTrigger::new()).build(),
        Activation::Portal => {
            let mode = if args.hold {
                ActivationMode::Hold
            } else {
                ActivationMode::Toggle
            };
            match GlobalShortcutTrigger::bind("dictate", args.shortcut.as_deref(), mode).await {
                Ok(trigger) => builder.trigger(trigger).build(),
                Err(e) => {
                    eprintln!("cannot bind the GlobalShortcuts portal: {e}");
                    eprintln!("  (the portal only serves packaged apps; pass --control to use the");
                    eprintln!("   control socket + a desktop custom shortcut instead)");
                    return ExitCode::FAILURE;
                }
            }
        }
        Activation::Control => match ControlTrigger::bind(control_path(&args)) {
            Ok(trigger) => builder.trigger(trigger).build(),
            Err(e) => {
                eprintln!(
                    "cannot bind control socket {}: {e}",
                    control_path(&args).display()
                );
                return ExitCode::FAILURE;
            }
        },
    };

    banner(&args);
    controller.run().await;
    println!("bye");
    ExitCode::SUCCESS
}

fn banner(args: &Args) {
    let sock = args
        .socket
        .as_ref()
        .map(|s| s.display().to_string())
        .unwrap_or_default();
    match Activation::resolve(args.activation) {
        Activation::Stdin => println!(
            "myna-desktop → {sock} — DEBUG stdin: Enter to start/stop (injects into THIS terminal)"
        ),
        Activation::Portal => {
            let verb = if args.hold { "hold" } else { "tap" };
            let key = args.shortcut.as_deref().unwrap_or("your chosen shortcut");
            println!("myna-desktop → {sock} — {verb} {key} to talk (portal)");
        }
        Activation::Control => {
            println!(
                "myna-desktop → {sock} — daemon ready; tap your dictation shortcut to start/stop."
            );
            println!(
                "  if you haven't bound one yet: `myna-desktop --install-shortcut '<Super>t>'"
            );
            println!(
                "  or bind a GNOME custom shortcut to: `{}`",
                toggle_command()
            );
        }
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

/// `--install-shortcut <accel>`: bind a GNOME custom keybinding to
/// `myna-desktop --toggle`, appending to any existing custom keybindings
/// (never clobbering).
fn install_shortcut(accel: &str) -> ExitCode {
    use std::process::Command;
    const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/myna/";
    let kb_schema = format!("{SCHEMA}.custom-keybinding:{PATH}");
    let command = toggle_command();

    let gset = |args: &[&str]| {
        Command::new("gsettings")
            .args(args)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };

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
                eprintln!(
                    "cannot reach the myna-desktop daemon at {}: {e}",
                    path.display()
                );
                eprintln!("  (is `myna-desktop --socket <path>` running?)");
                ExitCode::FAILURE
            }
        };
    }

    if args.socket.is_none() {
        eprintln!("--socket is required to run the daemon\n\n{USAGE}");
        return ExitCode::FAILURE;
    }

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
    if args.no_dbus {
        rt.block_on(run_controller(args, NotifyIndicator::new(), None, None))
    } else {
        rt.block_on(run_headless_dbus(args))
    }
}

/// The default indicator path: serve `org.myna.Dictation` and publish through
/// it (feature 004 — the GNOME Shell extension consumes it). Falls back to
/// desktop notifications by itself when the session bus is unreachable or the
/// name can't be owned, which is why this needs no flag — dictation never
/// hard-fails on it (P15). `--no-dbus` forces the notification path.
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
        Err(ServeError::AlreadyRunning { owner_pid }) => {
            let who = owner_pid
                .map(|pid| format!(" (pid {pid})"))
                .unwrap_or_default();
            eprintln!("a myna-desktop daemon is already running{who} - not starting a second one.");
            eprintln!("  a second daemon is worse than none: the GlobalShortcuts portal keeps the");
            eprintln!("  hotkey with whoever bound it first, so this process would never see a");
            eprintln!("  press while shadowing the running one.");
            eprintln!("  stop it first, or run this one with --no-dbus for a deliberate second.");
            ExitCode::FAILURE
        }
        Err(ServeError::Bus(e)) => {
            eprintln!("cannot serve org.myna.Dictation ({e}); falling back to notifications");
            eprintln!(
                "  (a 'GUID mismatch' means DBUS_SESSION_BUS_ADDRESS is stale - e.g. a tmux/screen"
            );
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
    worker.join().unwrap_or(ExitCode::FAILURE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> std::iter::Peekable<impl Iterator<Item = String>> {
        v.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .peekable()
    }

    #[test]
    fn install_shortcut_requires_accel() {
        // Missing accel must fail — no default.
        let result = parse_args_from(args(&["--install-shortcut"]));
        assert!(
            result.is_err(),
            "--install-shortcut without accel should fail"
        );
        assert!(result
            .unwrap_err()
            .contains("--install-shortcut needs a value"));
    }

    #[test]
    fn install_shortcut_with_accel_succeeds() {
        let result = parse_args_from(args(&["--install-shortcut", "<Super>t"]));
        assert!(
            result.is_ok(),
            "--install-shortcut with accel should succeed"
        );
        assert_eq!(
            result.unwrap().install_shortcut,
            Some("<Super>t".to_string())
        );
    }

    #[test]
    fn install_shortcut_accel_not_consumed_as_next_flag() {
        // Accel that looks like a flag (starts with '-') should still be accepted
        // because --install-shortcut always consumes the next arg.
        let result = parse_args_from(args(&["--install-shortcut", "--not-a-flag"]));
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().install_shortcut,
            Some("--not-a-flag".to_string())
        );
    }

    #[test]
    fn portal_shortcut_defaults_to_none() {
        // Without --shortcut, portal mode lets the portal dialog pick the key.
        let result = parse_args_from(args(&["--portal", "--socket", "/tmp/x.sock"]));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().shortcut, None);
    }

    // ── resolved-not-asked switches ─────────────────────────────────────────

    #[test]
    fn activation_defaults_to_unforced() {
        // No activation flag must stay `None` so `Activation::resolve` — not
        // the parser — decides, and the same argv resolves differently packaged
        // vs not.
        let a = parse_args_from(args(&["--socket", "/tmp/x.sock"])).unwrap();
        assert_eq!(a.activation, None);
    }

    #[test]
    fn activation_resolves_from_packaging() {
        // $SNAP is the portal's availability test: GNOME grants a portal app
        // identity only to a packaged app.
        assert_eq!(Activation::resolve(None), {
            if std::env::var_os("SNAP").is_some() {
                Activation::Portal
            } else {
                Activation::Control
            }
        });
        // An explicit flag always wins over the packaging default.
        assert_eq!(
            Activation::resolve(Some(Activation::Stdin)),
            Activation::Stdin
        );
    }

    #[test]
    fn conflicting_activation_flags_are_rejected() {
        // Last-one-wins would make `--portal --stdin` look like it worked.
        let err =
            parse_args_from(args(&["--portal", "--stdin", "--socket", "/tmp/x.sock"])).unwrap_err();
        assert!(err.contains("conflicting activation flags"), "{err}");
        // Repeating the same flag is harmless, not a conflict.
        assert!(
            parse_args_from(args(&["--portal", "--portal", "--socket", "/tmp/x.sock"])).is_ok()
        );
    }

    #[test]
    fn hold_requires_portal_activation() {
        // hold-to-talk needs press *and* release; the control socket only ever
        // delivers a single poke, so this must fail loudly rather than no-op.
        let err =
            parse_args_from(args(&["--control", "--hold", "--socket", "/tmp/x.sock"])).unwrap_err();
        assert!(err.contains("--hold only applies to portal"), "{err}");
        assert!(parse_args_from(args(&["--portal", "--hold", "--socket", "/tmp/x.sock"])).is_ok());
    }

    #[test]
    fn dbus_is_the_default_and_no_dbus_opts_out() {
        // The bus is served unless explicitly refused: it degrades to
        // notifications on its own, so there is nothing for a user to choose.
        assert!(
            !parse_args_from(args(&["--socket", "/tmp/x.sock"]))
                .unwrap()
                .no_dbus
        );
        assert!(
            parse_args_from(args(&["--no-dbus", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .no_dbus
        );
        // The old `--dbus` spelling is gone, not silently accepted.
        assert!(parse_args_from(args(&["--dbus", "--socket", "/tmp/x.sock"])).is_err());
    }

    #[test]
    fn preedit_is_tri_state() {
        // Unset means "resolve from the persisted mode", which is distinct from
        // an explicit off — `--no-preedit` must survive a streaming tier.
        assert_eq!(
            parse_args_from(args(&["--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            None
        );
        assert_eq!(
            parse_args_from(args(&["--preedit", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            Some(true)
        );
        assert_eq!(
            parse_args_from(args(&["--no-preedit", "--socket", "/tmp/x.sock"]))
                .unwrap()
                .preedit,
            Some(false)
        );
    }

    #[test]
    fn forced_preedit_skips_the_tier_gate() {
        // Overrides must not consult settings/tier state at all — that is what
        // makes them usable for debugging on any machine.
        assert!(resolve_preedit(Some(true)));
        assert!(!resolve_preedit(Some(false)));
    }

    #[test]
    fn portal_shortcut_explicit() {
        let result = parse_args_from(args(&[
            "--portal",
            "--shortcut",
            "<Super>t",
            "--socket",
            "/tmp/x.sock",
        ]));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().shortcut, Some("<Super>t".to_string()));
    }

    // Regression (manual test report, 2026-07-31): the HUD pill visibly
    // "blinked" on (re)start — recording→loading→recording published in
    // quick succession because `readiness.reset()` used to run lazily inside
    // the session's `async move` block, only once that future was first
    // polled. The controller publishes `IndicatorState::Recording`
    // *synchronously* right after `session.start()` returns (before the
    // future is ever polled), so a `ready_seen` left `true` from the
    // *previous* utterance made that first publish read `"recording"`
    // instead of the correct cold-start `"loading"`.
    //
    // `make_session`'s returned factory must reset `readiness` synchronously,
    // the moment it's called — provably before the returned `run` future is
    // ever polled/awaited.
    #[test]
    fn make_session_resets_readiness_synchronously_before_run_is_polled() {
        let args = Args {
            socket: Some(PathBuf::from("/tmp/myna-desktop-test-unused.sock")),
            ..Default::default()
        };
        let readiness = Readiness::new();
        // Simulate a previous utterance that reached Ready — the exact stale
        // state that triggered the flicker.
        readiness.note_ready();
        assert!(
            readiness.ready_seen(),
            "test setup: readiness should start warm"
        );

        let mut factory = make_session(&args, Some(readiness.clone()), None);
        let (events_tx, _events_rx) = mpsc::channel(1);
        // Calling the factory is synchronous; `run` below is deliberately
        // never polled/awaited, proving the reset can't be hiding inside it.
        let (_run, _stop) = factory(events_tx);

        assert!(
            !readiness.ready_seen(),
            "readiness must already be reset by the time session.start() \
             returns, before the controller's next set_state(Recording) \
             call — not lazily inside the unpolled session future"
        );
    }
}
