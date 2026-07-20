//! `myna-desktop` — the push-to-talk dictation app (plan T21/T22, T020/T025/T030).
//!
//! Composes activation (the GlobalShortcuts hotkey with `--hotkey`, else the
//! `StdinTrigger` MVP stand-in), the IBus text injector, and an activity
//! indicator over a live `myna-audio` PipeWire capture source and the
//! `myna-orchestrator` session, into a [`DesktopController`].
//!
//! With the `ui-gtk` feature (default), the persistent GTK4 overlay indicator
//! runs on the process **main thread** (GTK's requirement) while the controller
//! (tokio) runs on a worker thread; the two are bridged by an `async-channel`
//! (R6). Without `ui-gtk`, a headless `NotifyIndicator` runs on a plain tokio
//! runtime.
//!
//! ```text
//!   myna-server --adapter whisper --socket /tmp/myna.sock &
//!   myna-desktop --socket /tmp/myna.sock --hotkey --language en
//!   # focus a text field, hold Super+D, speak, release → text is injected
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use myna_audio::{CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::shortcut::portal::GlobalShortcutTrigger;
use myna_desktop::{DesktopController, Indicator};
use myna_orchestrator::{run_dictation, OrchestratorEvent, StdinTrigger, StopHandle, WsUnixBackend};
use tokio::sync::mpsc;

const USAGE: &str = "\
myna-desktop — push-to-talk dictation (T21/T22)

USAGE:
    myna-desktop --socket <path> [--hotkey] [--language <lang>] [--target <node>]

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required)
    --hotkey           activate hands-free via the GlobalShortcuts portal
                       (hold-to-talk); default binding Super+D, confirm/rebind
                       in the desktop's own shortcut dialog on first run.
                       Without it, use Enter/Enter on stdin (the MVP stand-in).
    --language <lang>  language hint sent in the session config (e.g. en)
    --target <node>    PipeWire node.name to capture from (default: system default)
    -h, --help         show this help

Hold the shortcut (or Enter) to start an utterance, release (or Enter) to stop.
The transcript is injected via IBus into the field focused when you started.
";

struct Args {
    socket: PathBuf,
    hotkey: bool,
    language: Option<String>,
    target: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut hotkey = false;
    let mut language = None;
    let mut target = None;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--hotkey" => hotkey = true,
            "--language" => language = Some(next(&mut it, "--language")?),
            "--target" => target = Some(next(&mut it, "--target")?),
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(Args {
        socket: socket.ok_or_else(|| format!("--socket is required\n\n{USAGE}"))?,
        hotkey,
        language,
        target,
    })
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Build the per-Press session factory: a fresh backend connection + live
/// capture source, run through the orchestrator (capture-at-press, ready-gated).
///
/// NOTE (T21 follow-up): negotiate `input_format` from server capabilities
/// (capabilities.query) rather than assuming the default; the shipped adapters
/// accept the default 16 kHz s16le mono, so the MVP uses it directly.
fn make_session(
    args: &Args,
) -> impl FnMut(mpsc::Sender<OrchestratorEvent>) -> (SessionRun, StopHandle) + Send + 'static {
    let socket = args.socket.clone();
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
        let run: SessionRun = Box::pin(async move {
            let mut sink = ChannelSink(events);
            run_dictation(&backend, config, source, &mut sink).await
        });
        (run, stop)
    }
}

/// Build and run the controller with the given indicator (tokio side).
async fn run_controller(args: Args, indicator: impl Indicator + 'static) -> ExitCode {
    let injector = match IbusInjector::connect().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot connect to IBus: {e}");
            eprintln!("  (is an IBus daemon running? check `ibus-daemon` / `echo $IBUS_ADDRESS`)");
            return ExitCode::FAILURE;
        }
    };

    let builder =
        DesktopController::builder().injector(injector).indicator(indicator).session(make_session(&args));

    let mut controller = if args.hotkey {
        match GlobalShortcutTrigger::bind("dictate", Some("SUPER+d")).await {
            Ok(trigger) => builder.trigger(trigger).build(),
            Err(e) => {
                eprintln!("cannot bind the global shortcut: {e}");
                eprintln!("  (is xdg-desktop-portal with a GlobalShortcuts backend running?)");
                return ExitCode::FAILURE;
            }
        }
    } else {
        builder.trigger(StdinTrigger::new()).build()
    };

    controller.run().await;
    ExitCode::SUCCESS
}

fn banner(args: &Args) {
    println!(
        "myna-desktop → {} — {}, then speak; the transcript is injected into the focused field",
        args.socket.display(),
        if args.hotkey { "hold Super+D" } else { "press Enter to start, Enter to stop, Ctrl-D to quit" }
    );
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    banner(&args);

    #[cfg(feature = "ui-gtk")]
    {
        run_with_gtk(args)
    }
    #[cfg(not(feature = "ui-gtk"))]
    {
        run_headless(args)
    }
}

/// Headless path: a plain tokio runtime + the `NotifyIndicator`.
#[cfg(not(feature = "ui-gtk"))]
fn run_headless(args: Args) -> ExitCode {
    use myna_desktop::indicator::notify::NotifyIndicator;
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = rt.block_on(run_controller(args, NotifyIndicator::new()));
    println!("bye");
    code
}

/// GTK path (R6): GTK owns the main thread + GLib loop; the controller runs on a
/// worker thread with a `GtkIndicator` bridged over an `async-channel`. When the
/// session loop ends the sender drops, closing the channel, which quits the GTK
/// app (see `run_indicator_app`).
#[cfg(feature = "ui-gtk")]
fn run_with_gtk(args: Args) -> ExitCode {
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
        rt.block_on(run_controller(args, GtkIndicator::new(tx)))
    });

    // Blocks in the GLib main loop until the channel closes (session ended).
    let _gtk_code = run_indicator_app(rx);
    let code = worker.join().unwrap_or(ExitCode::FAILURE);
    println!("bye");
    code
}
