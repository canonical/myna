//! `myna-dictate` — the orchestrator demo binary (plan T41): the Rust analogue
//! of `dev/dictate.py`. It wires the three boundary mocks to the wire-agnostic
//! FSM and runs push-to-talk against a real backend:
//!
//! ```text
//!   StdinTrigger ──press/release──▶ ┌─────────────┐
//!   WavFileSource ──PCM chunks────▶ │  FSM driver │ ──WS/UDS──▶ myna-server
//!   StdoutSink   ◀──transcript──── └─────────────┘
//! ```
//!
//! Run it against a live Python `myna-server` (any adapter):
//!
//! ```text
//!   myna-server --adapter fake --socket /tmp/myna.sock &
//!   myna-dictate --socket /tmp/myna.sock --clip corpus/real/audio/<id>.wav
//!   myna-dictate --socket /tmp/myna.sock --mic          # live microphone (T52, native PipeWire)
//! ```
//!
//! Press Enter to start an utterance, Enter again to stop (or let the clip play
//! out); the transcript prints. `Ctrl-D` quits. `--corpus <dir>` cycles through
//! a corpus manifest instead of a single clip. `--mic` captures live audio via
//! `myna-audio` (native PipeWire backend): capture starts at press and buffers in
//! the pre-ready ring while the model loads, so nothing said is lost.

use std::path::PathBuf;
use std::process::ExitCode;
use std::io::Write as _;
use std::time::Duration;

use async_trait::async_trait;
use myna_audio::{AudioStats, CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_orchestrator::{
    run_dictation, BackendClient, SessionOutcome, StdinTrigger, StdoutSink, TextSink, Trigger,
    TriggerEdge, WavFileSource, WsUnixBackend, WsUnixIe115Backend,
};
use tokio::sync::watch;

struct Args {
    socket: PathBuf,
    clips: Vec<Clip>,
    mic: bool,
    target: Option<String>,
    language: Option<String>,
    realtime: bool,
    dialect: Dialect,
    base64_audio: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Dialect {
    /// The internal `myna.core` wire (`session.start` / `transcription.*`).
    Internal,
    /// The OpenAI-Realtime-shaped IE115 wire (`session.update` / `status` /
    /// `conversation.item.input_audio_transcription.*`) — plan T43.
    Ie115,
}

struct Clip {
    path: PathBuf,
    reference: Option<String>,
}

const USAGE: &str = "\
myna-dictate — orchestrator push-to-talk demo (T41)

USAGE:
    myna-dictate --socket <path> (--clip <wav> | --corpus <dir> | --mic) [options]

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required)
    --clip <wav>       a single PCM WAV clip to dictate (repeatable)
    --corpus <dir>     a corpus dir with manifest.json; cycles its clips
    --mic              capture the live microphone (myna-audio / native PipeWire)
    --target <node>    PipeWire node.name to capture from (with --mic)
    --list-devices     list input devices (stable node.name + label) and exit;
                       stays live — plug/unplug updates the list until Ctrl-C
    --language <lang>  language hint sent in the session config (e.g. en)
    --dialect <name>   wire dialect: `internal` (default) or `ie115`
    --base64-audio     (ie115) send audio as base64 input_audio_buffer.append
                       frames instead of raw binary (OpenAI parity)
    --no-realtime      stream the clip as fast as possible (default: real-time)
    -h, --help         show this help
";

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut clips = Vec::new();
    let mut mic = false;
    let mut target = None;
    let mut language = None;
    let mut realtime = true;
    let mut dialect = Dialect::Internal;
    let mut base64_audio = false;
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--clip" => clips.push(Clip { path: PathBuf::from(next(&mut it, "--clip")?), reference: None }),
            "--corpus" => load_corpus(&PathBuf::from(next(&mut it, "--corpus")?), &mut clips)?,
            "--mic" => mic = true,
            "--target" => target = Some(next(&mut it, "--target")?),
            "--language" => language = Some(next(&mut it, "--language")?),
            "--dialect" => dialect = match next(&mut it, "--dialect")?.as_str() {
                "internal" => Dialect::Internal,
                "ie115" => Dialect::Ie115,
                other => return Err(format!("unknown dialect: {other} (want internal|ie115)")),
            },
            "--base64-audio" => base64_audio = true,
            "--no-realtime" => realtime = false,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    let socket = socket.ok_or_else(|| format!("--socket is required\n\n{USAGE}"))?;
    if mic && !clips.is_empty() {
        return Err("--mic and --clip/--corpus are mutually exclusive".into());
    }
    if !mic && clips.is_empty() {
        return Err(format!("one of --clip / --corpus / --mic is required\n\n{USAGE}"));
    }
    if target.is_some() && !mic {
        return Err("--target only applies to --mic".into());
    }
    if base64_audio && dialect != Dialect::Ie115 {
        return Err("--base64-audio only applies to --dialect ie115".into());
    }
    Ok(Args { socket, clips, mic, target, language, realtime, dialect, base64_audio })
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Read a corpus `manifest.json` (schema-v1, as written by
/// `dev/fetch_real_corpus.py`) and append its clips.
fn load_corpus(dir: &std::path::Path, clips: &mut Vec<Clip>) -> Result<(), String> {
    let manifest = dir.join("manifest.json");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("{}: {e}", manifest.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let entries = doc.get("clips").and_then(|c| c.as_array()).ok_or("manifest has no clips[]")?;
    for entry in entries {
        let rel = entry.get("path").and_then(|p| p.as_str()).ok_or("clip missing path")?;
        clips.push(Clip {
            path: dir.join(rel),
            reference: entry.get("text").and_then(|t| t.as_str()).map(String::from),
        });
    }
    if clips.is_empty() {
        return Err("corpus manifest lists no clips".into());
    }
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    // Standalone action: list input devices and exit (no socket needed).
    if std::env::args().skip(1).any(|a| a == "--list-devices") {
        return list_devices().await;
    }

    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // Same wire-agnostic run loop, either dialect: the FSM (and this driver)
    // don't change — only the BackendClient does (plan T43).
    match args.dialect {
        Dialect::Internal => dictate(WsUnixBackend::new(&args.socket), &args).await,
        Dialect::Ie115 => {
            dictate(WsUnixIe115Backend::new(&args.socket).base64_audio(args.base64_audio), &args).await
        }
    }
}

/// `--list-devices`: print the live input-device list (stable `node.name` +
/// human label) and keep updating it as devices appear/disappear until Ctrl-C
/// (feature 002-native-pipewire-backend, US4).
async fn list_devices() -> ExitCode {
    let devices = match myna_audio::InputDevices::new() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("cannot enumerate input devices: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut watch = devices.watch();
    // Give the registry a beat to deliver the initial set.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), watch.changed()).await;
    println!("input devices (Ctrl-C to stop watching):");
    let render = |list: &[myna_audio::InputDevice]| {
        if list.is_empty() {
            println!("  (none)");
        }
        for d in list {
            println!("  {}\t{}", d.node_name, d.label);
        }
    };
    render(&devices.list());
    loop {
        if watch.changed().await.is_err() {
            break;
        }
        println!("── devices changed ──");
        render(&devices.list());
    }
    ExitCode::SUCCESS
}

// ── VU meter ────────────────────────────────────────────────────────────────

/// Number of block-columns in the meter bar.
const METER_COLS: usize = 32;

/// Renders a live VU meter on the current terminal line (overwrites with `\r`).
/// Uses RMS for the fill level (smooth) and marks the per-chunk peak with `▎`.
/// Runs until the stats sender is dropped (i.e. the `CaptureSource` is done).
async fn render_meter(mut stats: watch::Receiver<AudioStats>) {
    loop {
        if stats.changed().await.is_err() {
            break;
        }
        let s = *stats.borrow_and_update();

        let filled = (s.rms.clamp(0.0, 1.0) * METER_COLS as f32).round() as usize;
        let peak_col = (s.peak.clamp(0.0, 1.0) * METER_COLS as f32).round() as usize;
        let bar: String = (0..METER_COLS)
            .map(|i| {
                if i < filled {
                    '█'
                } else if i == peak_col && peak_col >= filled {
                    '▎' // per-chunk peak needle
                } else {
                    '░'
                }
            })
            .collect();

        let db = if s.rms > 1e-7 { 20.0 * s.rms.log10() } else { -99.0f32 };
        let clip = if s.clipped { '!' } else { ' ' };
        print!("\r   ▐{bar}▌ {:>5.1} dBFS{clip}", db);
        let _ = std::io::stdout().flush();
    }
    // Leave the line clean so subsequent output isn't garbled.
    print!("\r\x1b[2K");
    let _ = std::io::stdout().flush();
}

/// Wraps [`StdoutSink`] and clears the VU-meter line before each `println!` so
/// the meter and session events don't overprint each other.
struct MicMeterSink(StdoutSink);

#[async_trait]
impl TextSink for MicMeterSink {
    async fn emit(&mut self, event: myna_orchestrator::fsm::OrchestratorEvent) {
        // Erase whatever the meter drew on this line.
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();
        self.0.emit(event).await;
    }
}

// ── dictation entry points ───────────────────────────────────────────────────

/// The push-to-talk loop, generic over the backend dialect.
async fn dictate<B: BackendClient>(backend: B, args: &Args) -> ExitCode {
    if args.mic {
        dictate_mic(backend, args).await
    } else {
        dictate_clips(backend, args).await
    }
}

/// Batch clip mode: run each clip in sequence and exit — no trigger needed.
async fn dictate_clips<B: BackendClient>(backend: B, args: &Args) -> ExitCode {
    let n = args.clips.len();
    let plural = if n == 1 { "" } else { "s" };
    println!("dictating {n} clip{plural} to {}", args.socket.display());

    let mut sink = StdoutSink;
    let mut exit = ExitCode::SUCCESS;

    for clip in &args.clips {
        if let Some(reference) = &clip.reference {
            println!("── clip {}: (reference: {reference})", clip.path.display());
        } else {
            println!("── clip {}", clip.path.display());
        }
        let source = match WavFileSource::new(&clip.path) {
            Ok(s) => s.realtime(args.realtime),
            Err(e) => {
                eprintln!("✗ cannot open {}: {e}", clip.path.display());
                exit = ExitCode::FAILURE;
                continue;
            }
        };
        let config = SessionConfig { language: args.language.clone(), ..Default::default() };
        match run_dictation(&backend, config, source, &mut sink).await {
            Ok(SessionOutcome::Completed { .. }) => {} // StdoutSink already printed it
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, message }) => {
                eprintln!("✗ session failed [{code}]: {message}");
                exit = ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("✗ could not open session: {e}");
                exit = ExitCode::FAILURE;
            }
        }
    }

    exit
}

/// Interactive push-to-talk mic mode: press Enter to speak, Enter to stop, Ctrl-D to quit.
async fn dictate_mic<B: BackendClient>(backend: B, args: &Args) -> ExitCode {
    let mut trigger = StdinTrigger::new();

    println!("dictating to {} — press Enter to speak, Enter again to stop, Ctrl-D to quit", args.socket.display());

    loop {
        // Idle: wait for a Press (skip stray releases; EOF quits).
        let pressed = loop {
            match trigger.next_edge().await {
                Some(TriggerEdge::Press) => break true,
                Some(TriggerEdge::Release) => continue,
                None => break false,
            }
        };
        if !pressed {
            break;
        }

        let mut builder = CaptureSource::builder(AudioFormat::default());
        if let Some(node) = &args.target {
            builder = builder.target(node.clone());
        }
        let source = builder.backend(Box::new(PipeWireBackend::new())).build();
        println!("── mic: capturing (Enter to stop)");
        let mic_stats = source.stats();
        let stop = source.stop_handle();
        let config = SessionConfig { language: args.language.clone(), ..Default::default() };

        // Start the VU meter: runs as a background task, writes \r bars,
        // and clears the line when the stats sender drops (session ends).
        let meter = tokio::spawn(render_meter(mic_stats.clone()));
        let mut sink = MicMeterSink(StdoutSink);

        // Run the utterance while watching for a Release edge (graceful early
        // stop → end-of-audio → finalize).
        let fut = run_dictation(&backend, config, source, &mut sink);
        tokio::pin!(fut);
        let mut quit = false;
        let outcome = loop {
            tokio::select! {
                result = &mut fut => break result,
                edge = trigger.next_edge() => match edge {
                    Some(TriggerEdge::Release) => stop.stop(),
                    Some(TriggerEdge::Press) => {} // ignore an extra press
                    None => { stop.stop(); quit = true; }
                }
            }
        };

        // Stop the meter and ensure the line is clear before any outcome text.
        meter.abort();
        let _ = meter.await;
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();

        match outcome {
            Ok(SessionOutcome::Completed { .. }) => {} // StdoutSink already printed it
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, message }) if code == "capture_failed" => {
                eprintln!("✗ audio capture failed: {message}");
                eprintln!("  (no mic? check `wpctl status` lists an Audio Source, or pass --target <node.name>)");
            }
            Ok(SessionOutcome::Failed { code, message }) => {
                eprintln!("✗ session failed [{code}]: {message}");
            }
            Err(e) => eprintln!("✗ could not open session: {e}"),
        }

        // The T51 acceptance readout: how much the mic captured and whether
        // the pre-ready ring aged anything out (zero drops expected).
        let s = *mic_stats.borrow();
        if s.dropped > Duration::ZERO {
            println!(
                "  (mic: captured {:.1?} — DROPPED {:.1?}: ring overflow, transcript starts mid-utterance)",
                s.captured, s.dropped
            );
        } else {
            println!("  (mic: captured {:.1?}, zero drops)", s.captured);
        }
        // Near-silent capture (~-40 dBFS peak) over a non-trivial window is
        // almost always a muted input or the wrong node, not a quiet
        // talker — flag it so an empty transcript isn't a mystery.
        if s.captured > Duration::from_millis(500) && s.session_peak < 0.01 {
            println!(
                "  ⚠ input was near-silent (peak {:.4}) — is the mic muted or the wrong device? check `wpctl status` / try --target <node.name>",
                s.session_peak
            );
        }

        if quit {
            break;
        }
    }

    println!("bye");
    ExitCode::SUCCESS
}
