//! `myna-desktop` — the push-to-talk dictation app (plan T21/T22, T020).
//!
//! Composes the MVP activation stand-in (the orchestrator's `StdinTrigger` —
//! Enter to start, Enter to stop; the real GlobalShortcuts hotkey is branch
//! 003c), the IBus text injector, and a headless `NotifyIndicator` (the GTK
//! overlay is branch 003d) over a live `myna-audio` PipeWire capture source and
//! the `myna-orchestrator` session, into a [`DesktopController`].
//!
//! ```text
//!   myna-server --adapter whisper --socket /tmp/myna.sock &
//!   myna-desktop --socket /tmp/myna.sock [--language en]
//!   # focus a text field, press Enter, speak, press Enter → text is injected
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use myna_core::{AudioFormat, SessionConfig};
use myna_desktop::controller::{ChannelSink, SessionRun};
use myna_desktop::indicator::notify::NotifyIndicator;
use myna_desktop::inject::ibus::IbusInjector;
use myna_desktop::DesktopController;
use myna_orchestrator::{
    run_dictation, OrchestratorEvent, StdinTrigger, StopHandle, WsUnixBackend,
};
use myna_audio::{CaptureSource, PipeWireBackend};
use tokio::sync::mpsc;

const USAGE: &str = "\
myna-desktop — push-to-talk dictation (T21/T22)

USAGE:
    myna-desktop --socket <path> [--language <lang>] [--target <node>]

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required)
    --language <lang>  language hint sent in the session config (e.g. en)
    --target <node>    PipeWire node.name to capture from (default: system default)
    -h, --help         show this help

Enter starts an utterance; Enter again stops it (Ctrl-D quits). The transcript
is injected via IBus into the field focused when you pressed Enter.
";

struct Args {
    socket: PathBuf,
    language: Option<String>,
    target: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
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
            "--language" => language = Some(next(&mut it, "--language")?),
            "--target" => target = Some(next(&mut it, "--target")?),
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    Ok(Args {
        socket: socket.ok_or_else(|| format!("--socket is required\n\n{USAGE}"))?,
        language,
        target,
    })
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let injector = match IbusInjector::connect().await {
        Ok(i) => i,
        Err(e) => {
            eprintln!("cannot connect to IBus: {e}");
            eprintln!("  (is an IBus daemon running? check `ibus-daemon` / `echo $IBUS_ADDRESS`)");
            return ExitCode::FAILURE;
        }
    };

    // One dictation session per Press: a fresh backend connection + live capture
    // source, run through the orchestrator (capture-at-press, ready-gated push).
    // NOTE (T21 follow-up): negotiate `input_format` from server capabilities
    // (capabilities.query) instead of assuming the default; the adapters we ship
    // accept the default 16 kHz s16le mono, so the MVP uses it directly.
    let socket = args.socket.clone();
    let language = args.language.clone();
    let target = args.target.clone();
    let session = move |events: mpsc::Sender<OrchestratorEvent>| -> (SessionRun, StopHandle) {
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
    };

    println!(
        "myna-desktop → {} — focus a text field, press Enter to speak, Enter to stop, Ctrl-D to quit",
        args.socket.display()
    );

    let mut controller = DesktopController::builder()
        .trigger(StdinTrigger::new())
        .injector(injector)
        .indicator(NotifyIndicator::new())
        .session(session)
        .build();

    controller.run().await;
    println!("bye");
    ExitCode::SUCCESS
}
