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
//! ```
//!
//! Press Enter to start an utterance, Enter again to stop (or let the clip play
//! out); the transcript prints. `Ctrl-D` quits. `--corpus <dir>` cycles through
//! a corpus manifest instead of a single clip.

use std::path::PathBuf;
use std::process::ExitCode;

use myna_orchestrator::{
    run_dictation, SessionOutcome, StdinTrigger, StdoutSink, Trigger, TriggerEdge, WavFileSource,
    WsUnixBackend,
};
use myna_core::SessionConfig;

struct Args {
    socket: PathBuf,
    clips: Vec<Clip>,
    language: Option<String>,
    realtime: bool,
}

struct Clip {
    path: PathBuf,
    reference: Option<String>,
}

const USAGE: &str = "\
myna-dictate — orchestrator push-to-talk demo (T41)

USAGE:
    myna-dictate --socket <path> (--clip <wav> | --corpus <dir>) [options]

OPTIONS:
    --socket <path>    Unix socket of a running myna-server (required)
    --clip <wav>       a single PCM WAV clip to dictate (repeatable)
    --corpus <dir>     a corpus dir with manifest.json; cycles its clips
    --language <lang>  language hint sent in the session config (e.g. en)
    --no-realtime      stream the clip as fast as possible (default: real-time)
    -h, --help         show this help
";

fn parse_args() -> Result<Args, String> {
    let mut socket = None;
    let mut clips = Vec::new();
    let mut language = None;
    let mut realtime = true;
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
            "--language" => language = Some(next(&mut it, "--language")?),
            "--no-realtime" => realtime = false,
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    let socket = socket.ok_or_else(|| format!("--socket is required\n\n{USAGE}"))?;
    if clips.is_empty() {
        return Err(format!("one of --clip / --corpus is required\n\n{USAGE}"));
    }
    Ok(Args { socket, clips, language, realtime })
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
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    let backend = WsUnixBackend::new(&args.socket);
    let mut trigger = StdinTrigger::new();
    let mut sink = StdoutSink;
    let mut next_clip = 0usize;

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

        let clip = &args.clips[next_clip % args.clips.len()];
        next_clip += 1;
        if let Some(reference) = &clip.reference {
            println!("── clip {}: (reference: {reference})", clip.path.display());
        } else {
            println!("── clip {}", clip.path.display());
        }

        let source = match WavFileSource::new(&clip.path) {
            Ok(s) => s.realtime(args.realtime),
            Err(e) => {
                eprintln!("✗ cannot open {}: {e}", clip.path.display());
                continue;
            }
        };
        let stop = source.stop_handle();
        let config = SessionConfig { language: args.language.clone(), ..Default::default() };

        // Run the utterance while watching for a Release edge (graceful early
        // stop → end-of-audio → finalize). The clip playing out has the same
        // effect on its own.
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

        match outcome {
            Ok(SessionOutcome::Completed { .. }) => {} // StdoutSink already printed it
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, message }) => {
                eprintln!("✗ session failed [{code}]: {message}");
            }
            Err(e) => eprintln!("✗ could not open session: {e}"),
        }

        if quit {
            break;
        }
    }

    println!("bye");
    ExitCode::SUCCESS
}
