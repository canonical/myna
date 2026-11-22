//! `myna-testbed` - drives backend sessions from WAV clips, a corpus or the
//! microphone. It wires the three boundary mocks to the wire-agnostic FSM and
//! runs push-to-talk against a real backend:
//!
//! ```text
//!   StdinTrigger ──press/release──▶ ┌─────────────┐
//!   WavFileSource ──PCM chunks────▶ │  FSM driver │ ──WS/UDS──▶ myna-server
//!   StdoutSink   ◀──transcript────  └─────────────┘
//! ```
//!
//! Run it against a live Python `myna-server` (any adapter):
//!
//! ```text
//!   myna-server --adapter fake --socket /tmp/myna.sock &
//!   myna-testbed --socket /tmp/myna.sock --clip corpus/english/audio/<id>.wav
//!   myna-testbed --socket /tmp/myna.sock --mic          # live microphone
//! ```
//!
//! Press Enter to start an utterance, Enter again to stop (or let the clip play
//! out); the transcript prints. `Ctrl-D` quits. `--corpus <dir>` cycles through
//! a corpus manifest instead of a single clip. `--mic` captures live audio via
//! `myna-audio` (native PipeWire backend): capture starts at press and buffers in
//! the pre-ready ring while the model loads, so nothing said is lost.

mod convert;
mod discover;
mod lab;
mod report;
mod run;
mod spec;

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use async_trait::async_trait;
use myna_audio::{AudioStats, CaptureSource, PipeWireBackend};
use myna_core::{AudioFormat, SessionConfig};
use myna_orchestrator::backend::share::BackendSocket;
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
    vocabulary: Vec<String>,
    realtime: bool,
    dialect: Dialect,
    base64_audio: bool,
    ws_path: Option<String>,
    show_unstable: bool,
    /// `--mode`; `None` defers to the user's choice, then the backend.
    mode: Option<myna_core::StreamingMode>,
    /// Set when graphs are to be timed (`--graph`, `--sweep`, `--text`).
    lab: Option<LabArgs>,
}

/// What the graph lab was asked for.
struct LabArgs {
    specs: Vec<spec::Spec>,
    purpose: myna_desktop::inject::FieldPurpose,
    mid_sentence: bool,
    /// Text to run instead of speech, one utterance each.
    text: Vec<String>,
    json: Option<PathBuf>,
    share_dirs: Vec<PathBuf>,
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
myna-testbed - drive a backend session from WAV clips, a corpus or the microphone,
               and time what the processing graphs do to the text

USAGE:
    myna-testbed [--socket <path> | --backend-dir <d>] (--clip <wav> | --corpus <dir> | --mic | --text <t>) [options]

    The speech backend defaults to the installed snap; text models are found
    as the daemon finds them (any connected or installed myna-text share).

EXAMPLES:
    myna-testbed --mic --graph tidy --graph tidy-french
    myna-testbed --text \"um I I think we ship, no, Tuesday\" --sweep disfluency,cleanup
    myna-testbed --corpus <dir> --limit 20 --graph tidy --json out.json

OPTIONS:
    --socket <path>    Unix socket of a running myna-server
    --backend-dir <d>  directory to find the backend socket under
                       (<d>/*/provider.env - how the snap wires the `backend`
                       content share). Default: the installed speech snap.
    --clip <wav>       a single PCM WAV clip to dictate (repeatable); any rate,
                       channel count or integer width, converted to 16 kHz mono
    --corpus <dir>     a corpus dir with manifest.json; cycles its clips
    --mic              capture the live microphone (myna-audio / native PipeWire)
    --target <node>    PipeWire node.name to capture from (with --mic)
    --list-devices     list input devices (stable node.name + label) and exit;
                       stays live — plug/unplug updates the list until Ctrl-C
    --language <lang>  language hint sent in the session config (e.g. en)
    --vocabulary <word> name or phrase the backend should bias recognition
                       toward (repeatable; e.g. --vocabulary Siobhan)
    --dialect <name>   wire dialect: `internal` (default) or `ie115`
    --base64-audio     (ie115) send audio as base64 input_audio_buffer.append
                       frames instead of raw binary (OpenAI parity)
    --ws-path <path>   (ie115) WebSocket endpoint path (default /; colleagues'
                       adapter uses /v1/realtime)
    --show-unstable    display unstable hypothesis deltas as `~` lines
                       (streaming mode; off by default — FR-007)
    --mode <mode>      transcription mode: `streaming` or `batch`; overrides
                       the persisted setting, which overrides the backend's
                       own mode
    --graph <spec>     run the speech through this graph and time it (repeatable).
                       A preset id (exact, tidy, tidy-french, keyword) or a comma
                       list of steps: disfluency, vocabulary, cleanup,
                       translate=FR (or EN:FR), address, continue, script=NICK.
                       `exact` is always run too, as the baseline.
    --sweep <steps>    run every ordered subset of the comma list (3 steps are
                       15 graphs; at most 5 steps)
    --field <kind>     the field typed into: plain (default), url, email, terminal
    --mid-sentence     the text continues a sentence already in the field
    --text <text>      run this text instead of speech (repeatable); `-` reads
                       one utterance per stdin line. Sentences are taken as
                       spoken 3 s apart, the last at the stop.
    --json <file>      also write every measure as JSON
    --text-share <d>   a directory of text shares to search too (repeatable;
                       MYNA_TEXT_SHARES works as for the daemon)
    --limit <n>        with --corpus: only the first n clips
    --only <substr>    with --corpus: only clips whose path contains substr
    --realtime         pace clips at real time, like a microphone (default: as
                       fast as the backend takes them)
    -h, --help         show this help
";

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut socket = None;
    let mut backend_dir = None;
    let mut clips = Vec::new();
    let mut mic = false;
    let mut target = None;
    let mut language = None;
    let mut vocabulary = Vec::new();
    let mut realtime = false;
    let mut dialect = Dialect::Internal;
    let mut base64_audio = false;
    let mut ws_path: Option<String> = None;
    let mut show_unstable = false;
    let mut mode = None;
    let mut graphs: Vec<String> = Vec::new();
    let mut sweep: Option<String> = None;
    let mut purpose = myna_desktop::inject::FieldPurpose::Plain;
    let mut mid_sentence = false;
    let mut text: Vec<String> = Vec::new();
    let mut json = None;
    let mut text_shares: Vec<PathBuf> = Vec::new();
    let mut limit: Option<usize> = None;
    let mut only: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--socket" => socket = Some(PathBuf::from(next(&mut it, "--socket")?)),
            "--backend-dir" => backend_dir = Some(PathBuf::from(next(&mut it, "--backend-dir")?)),
            "--clip" => clips.push(Clip {
                path: PathBuf::from(next(&mut it, "--clip")?),
                reference: None,
            }),
            "--corpus" => load_corpus(&PathBuf::from(next(&mut it, "--corpus")?), &mut clips)?,
            "--mic" => mic = true,
            "--target" => target = Some(next(&mut it, "--target")?),
            "--language" => language = Some(next(&mut it, "--language")?),
            "--vocabulary" => vocabulary.push(next(&mut it, "--vocabulary")?),
            "--dialect" => {
                dialect = match next(&mut it, "--dialect")?.as_str() {
                    "internal" => Dialect::Internal,
                    "ie115" => Dialect::Ie115,
                    other => return Err(format!("unknown dialect: {other} (want internal|ie115)")),
                }
            }
            "--base64-audio" => base64_audio = true,
            "--ws-path" => ws_path = Some(next(&mut it, "--ws-path")?),
            "--show-unstable" => show_unstable = true,
            "--mode" => {
                mode = Some(match next(&mut it, "--mode")?.as_str() {
                    "streaming" => myna_core::StreamingMode::Streaming,
                    "batch" => myna_core::StreamingMode::Batch,
                    other => return Err(format!("unknown mode: {other} (want streaming|batch)")),
                })
            }
            "--realtime" => realtime = true,
            "--graph" => graphs.push(next(&mut it, "--graph")?),
            "--sweep" => sweep = Some(next(&mut it, "--sweep")?),
            "--field" => {
                use myna_desktop::inject::FieldPurpose as P;
                purpose = match next(&mut it, "--field")?.as_str() {
                    "plain" => P::Plain,
                    "url" => P::Url,
                    "email" => P::Email,
                    "terminal" => P::Terminal,
                    other => {
                        return Err(format!(
                            "unknown field: {other} (want plain|url|email|terminal)"
                        ))
                    }
                }
            }
            "--mid-sentence" => mid_sentence = true,
            "--text" => text.push(next(&mut it, "--text")?),
            "--json" => json = Some(PathBuf::from(next(&mut it, "--json")?)),
            "--text-share" => text_shares.push(PathBuf::from(next(&mut it, "--text-share")?)),
            "--limit" => {
                limit = Some(
                    next(&mut it, "--limit")?
                        .parse()
                        .map_err(|_| "--limit needs a number".to_string())?,
                )
            }
            "--only" => only = Some(next(&mut it, "--only")?),
            other => return Err(format!("unknown argument: {other}\n\n{USAGE}")),
        }
    }
    if let Some(only) = &only {
        clips.retain(|c| c.path.to_string_lossy().contains(only.as_str()));
    }
    if let Some(limit) = limit {
        clips.truncate(limit);
    }
    if (only.is_some() || limit.is_some()) && clips.is_empty() && !mic {
        return Err("--limit/--only left no clips (they apply to --corpus)".into());
    }
    if text.iter().any(|t| t == "-") {
        text.retain(|t| t != "-");
        let mut piped = String::new();
        std::io::stdin()
            .read_to_string(&mut piped)
            .map_err(|e| format!("stdin: {e}"))?;
        text.extend(
            piped
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_owned),
        );
    }
    if (mic || !clips.is_empty()) && !text.is_empty() {
        return Err("--text is an alternative to --mic and --clip/--corpus".into());
    }
    if mic && !clips.is_empty() {
        return Err("--mic and --clip/--corpus are mutually exclusive".into());
    }
    if !mic && clips.is_empty() && text.is_empty() {
        return Err(format!(
            "one of --clip / --corpus / --mic / --text is required\n\n{USAGE}"
        ));
    }
    if target.is_some() && !mic {
        return Err("--target only applies to --mic".into());
    }
    if base64_audio && dialect != Dialect::Ie115 {
        return Err("--base64-audio only applies to --dialect ie115".into());
    }
    if ws_path.is_some() && dialect != Dialect::Ie115 {
        return Err("--ws-path only applies to --dialect ie115".into());
    }
    let wants_lab = !graphs.is_empty() || sweep.is_some() || !text.is_empty();
    if !wants_lab
        && (json.is_some() || mid_sentence || purpose != myna_desktop::inject::FieldPurpose::Plain)
    {
        return Err("--json, --field and --mid-sentence need --graph, --sweep or --text".into());
    }
    // Speech is needed unless text stands in for it.
    let backend = match BackendSocket::from_flags(socket, backend_dir)? {
        Some(backend) => Some(backend),
        None if text.is_empty() => Some(discover::default_backend(discover::var_snap())?),
        None => None,
    };
    let socket = match &backend {
        Some(backend) => resolve_socket(backend)?,
        None => PathBuf::new(),
    };
    let lab = if wants_lab {
        let mut labels = graphs;
        if let Some(items) = &sweep {
            labels.extend(spec::sweep(items)?);
        }
        if labels.is_empty() {
            labels.push("tidy".into());
        }
        if !labels.iter().any(|l| l.trim() == "exact") {
            labels.insert(0, "exact".into());
        }
        let specs = labels
            .iter()
            .map(|l| spec::parse(l).map_err(|e| format!("--graph {l}: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        let search = match &backend {
            Some(BackendSocket::Search(dir)) => Some(dir.as_path()),
            _ => None,
        };
        let mut share_dirs = myna_desktop::process::resolve::share_dirs(
            search,
            std::env::var_os("MYNA_TEXT_SHARES"),
        );
        share_dirs.extend(text_shares);
        if discover::myna_snap_data().is_none() {
            share_dirs.extend(discover::snap_share_dirs(discover::var_snap()));
        }
        let mut seen = std::collections::HashSet::new();
        share_dirs.retain(|d| seen.insert(d.clone()));
        Some(LabArgs {
            specs,
            purpose,
            mid_sentence,
            text,
            json,
            share_dirs,
        })
    } else {
        None
    };

    Ok(Args {
        socket,
        clips,
        mic,
        target,
        language,
        vocabulary,
        realtime,
        dialect,
        base64_audio,
        ws_path,
        show_unstable,
        mode,
        lab,
    })
}

/// The socket to use; on failure, the translated headline and, on the next
/// line, the untranslated detail naming where it looked.
fn resolve_socket(backend: &BackendSocket) -> Result<PathBuf, String> {
    let provider = backend.resolve().map_err(|e| match backend {
        BackendSocket::Search(dir) => format!("{}\n{}: {e}", e.headline(), dir.display()),
        BackendSocket::Fixed(_) => format!("{}\n{e}", e.headline()),
    })?;
    Ok(provider.socket)
}

/// A clip as a source in the format the backend takes, whatever it was
/// recorded in.
fn open_clip(path: &std::path::Path) -> Result<WavFileSource, String> {
    use myna_orchestrator::AudioSource as _;
    let wav =
        WavFileSource::new(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let target = AudioFormat::default();
    let pcm = convert::convert(wav.format(), wav.pcm(), target)
        .map_err(|e| format!("cannot convert {}: {e}", path.display()))?;
    Ok(WavFileSource::from_pcm(target, pcm.into()))
}

fn next(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    it.next().ok_or_else(|| format!("{flag} needs a value"))
}

/// Read a corpus `manifest.json` (schema-v1, as written by
/// `myna-bench download-corpus`) and append its clips.
///
/// Every clip is hashed against the digest the manifest records. Audio that
/// is not what the manifest says it is stops the run: a transcript printed
/// beside the wrong reference is worse than no transcript.
fn load_corpus(dir: &std::path::Path, clips: &mut Vec<Clip>) -> Result<(), String> {
    let manifest = dir.join("manifest.json");
    let text =
        std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let doc: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let corpus_id = doc
        .get("corpus_id")
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            format!(
                "{}: no corpus_id - regenerate the corpus",
                manifest.display()
            )
        })?;
    let entries = doc
        .get("clips")
        .and_then(|c| c.as_array())
        .ok_or("manifest has no clips[]")?;
    let mut drifted = Vec::new();
    for entry in entries {
        let rel = entry
            .get("path")
            .and_then(|p| p.as_str())
            .ok_or("clip missing path")?;
        let id = entry.get("id").and_then(|i| i.as_str()).unwrap_or(rel);
        let recorded = entry
            .get("sha256")
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                format!(
                    "{}: clip {id} records no sha256 - regenerate the corpus",
                    manifest.display()
                )
            })?;
        let path = dir.join(rel);
        let found = sha256_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if found != recorded {
            drifted.push(format!("  {id}: {} != {}", &found[..16], &recorded[..16]));
        }
        clips.push(Clip {
            path,
            reference: entry.get("text").and_then(|t| t.as_str()).map(String::from),
        });
    }
    if !drifted.is_empty() {
        return Err(format!(
            "{}: {} clip(s) are not what the manifest records - regenerate the corpus\n{}",
            manifest.display(),
            drifted.len(),
            drifted.join("\n")
        ));
    }
    if clips.is_empty() {
        return Err("corpus manifest lists no clips".into());
    }
    println!("corpus {corpus_id} ({} clips)", entries.len());
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String, std::io::Error> {
    use sha2::{Digest as _, Sha256};

    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hasher.finalize().iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    }))
}

#[tokio::main]
async fn main() -> ExitCode {
    // Standalone action: list input devices and exit (no socket needed).
    if std::env::args().skip(1).any(|a| a == "--list-devices") {
        return list_devices().await;
    }

    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };

    // Typed text needs no speech backend at all.
    if let Some(lab) = args.lab.as_ref().filter(|l| !l.text.is_empty()) {
        return text_lab(lab, &args).await;
    }

    // Same wire-agnostic run loop, either dialect: the FSM (and this driver)
    // don't change — only the BackendClient does (plan T43).
    match args.dialect {
        Dialect::Internal => dictate(WsUnixBackend::new(&args.socket), &args).await,
        Dialect::Ie115 => {
            let mut backend = WsUnixIe115Backend::new(&args.socket).base64_audio(args.base64_audio);
            if let Some(ref path) = args.ws_path {
                backend = backend.ws_path(path);
            }
            dictate(backend, &args).await
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

        let db = if s.rms > 1e-7 {
            20.0 * s.rms.log10()
        } else {
            -99.0f32
        };
        let clip = if s.clipped { '!' } else { ' ' };
        print!("\r   ▐{bar}▌ {:>5.1} dBFS{clip}", db);
        let _ = std::io::stdout().flush();
    }
    // Leave the line clean so subsequent output isn't garbled.
    print!("\r\x1b[2K");
    let _ = std::io::stdout().flush();
}

/// T036 (feature 007): drops `Unstable` hypothesis events unless the user
/// opted in with --show-unstable. Committed text always passes through.
struct UnstableFilter<T> {
    inner: T,
    show_unstable: bool,
}

#[async_trait]
impl<T: TextSink> TextSink for UnstableFilter<T> {
    async fn emit(&mut self, event: myna_orchestrator::fsm::OrchestratorEvent) {
        if !self.show_unstable
            && matches!(
                event,
                myna_orchestrator::fsm::OrchestratorEvent::Unstable(_)
            )
        {
            return; // FR-007: unstable text is never shown by default
        }
        self.inner.emit(event).await;
    }
}

/// T038 (feature 007): batch display mode — progressive committed `Final`
/// segments are swallowed; the terminal `Done` (full transcript) is the only
/// text shown. Unstable/AudioDropped/lifecycle still pass through.
struct BatchDisplay<T>(T);

#[async_trait]
impl<T: TextSink> TextSink for BatchDisplay<T> {
    async fn emit(&mut self, event: myna_orchestrator::fsm::OrchestratorEvent) {
        if matches!(event, myna_orchestrator::fsm::OrchestratorEvent::Final(_)) {
            return;
        }
        self.0.emit(event).await;
    }
}

/// What shows the session as it runs: the meter and transcript, or nothing
/// when the lab prints the result itself.
enum Printer {
    Meter(MicMeterSink),
    Quiet(run::Quiet),
}

#[async_trait]
impl TextSink for Printer {
    async fn emit(&mut self, event: myna_orchestrator::fsm::OrchestratorEvent) {
        match self {
            Printer::Meter(sink) => sink.emit(event).await,
            Printer::Quiet(sink) => sink.emit(event).await,
        }
    }
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
    let mut lab = args.lab.as_ref().map(|l| make_lab(l, args));
    if let Some(lab) = lab.as_mut() {
        lab.warm().await;
    }
    let exit = if args.mic {
        dictate_mic(backend, args, lab.as_mut()).await
    } else if let Some(lab) = lab.as_mut() {
        dictate_clips_lab(backend, args, lab).await
    } else {
        dictate_clips(backend, args).await
    };
    match lab.as_ref().map(run::Lab::finish) {
        Some(Err(e)) => {
            eprintln!("✗ {e}");
            ExitCode::FAILURE
        }
        _ => exit,
    }
}

fn make_lab(lab: &LabArgs, args: &Args) -> run::Lab {
    let total = if lab.text.is_empty() {
        args.clips.len()
    } else {
        lab.text.len()
    };
    run::Lab::new(
        run::Config {
            specs: lab.specs.clone(),
            purpose: lab.purpose,
            mid_sentence: lab.mid_sentence,
            vocabulary: args.vocabulary.clone(),
            language: args.language.clone(),
            share_dirs: lab.share_dirs.clone(),
            json: lab.json.clone(),
        },
        total,
    )
}

/// `--text`: no speech, so only the graphs run.
async fn text_lab(lab_args: &LabArgs, args: &Args) -> ExitCode {
    let mut lab = make_lab(lab_args, args);
    lab.warm().await;
    for (i, text) in lab_args.text.iter().enumerate() {
        lab.analyse(lab::Speech::typed(format!("text {}", i + 1), text))
            .await;
    }
    match lab.finish() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("✗ {e}");
            ExitCode::FAILURE
        }
    }
}

/// Clips through the speech backend once each, then every graph over what
/// was heard.
async fn dictate_clips_lab<B: BackendClient>(
    backend: B,
    args: &Args,
    lab: &mut run::Lab,
) -> ExitCode {
    let mut exit = ExitCode::SUCCESS;
    let realtime = if args.realtime {
        "realtime"
    } else {
        "flat out: pass --realtime to feel the wait"
    };
    println!("speech: {} ({realtime})", args.socket.display());
    for clip in &args.clips {
        let wav = match open_clip(&clip.path) {
            Ok(s) => s.realtime(args.realtime),
            Err(e) => {
                eprintln!("✗ {e}");
                exit = ExitCode::FAILURE;
                continue;
            }
        };
        let stop = run::Stop::default();
        let source = run::Stamped::new(wav, stop.clone());
        let config = SessionConfig {
            language: args.language.clone(),
            keywords: args.vocabulary.clone(),
            ..Default::default()
        };
        let mut sink = run::Recording::new(run::Quiet);
        match run_dictation(&backend, config, source, &mut sink).await {
            Ok(SessionOutcome::Completed { .. }) => {
                let id = clip.path.file_name().map_or_else(
                    || clip.path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                lab.analyse(lab::Speech::recorded(
                    id,
                    clip.reference.clone(),
                    &sink.events,
                    stop.at(),
                ))
                .await;
            }
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, failure }) => {
                eprintln!("✗ {} [{code}]\n  {}", failure.headline, failure.detail);
                exit = ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("✗ {}\n  could not open session: {e}", e.headline());
                exit = ExitCode::FAILURE;
            }
        }
    }
    exit
}

/// The mode the clips run in and why: `--mode`, else the user's stored
/// choice, else the backend's own. The backend is asked only when it decides.
async fn clip_mode(
    flag: Option<myna_core::StreamingMode>,
    stored: impl FnOnce() -> Option<myna_core::StreamingMode>,
    socket: &std::path::Path,
) -> (myna_core::StreamingMode, String) {
    if let Some(mode) = flag {
        return (mode, format!("streaming-mode {mode:?}, from --mode"));
    }
    let stored = stored();
    let streams = match stored {
        Some(_) => None,
        None => match myna_orchestrator::query_capabilities(socket).await {
            Ok(caps) => caps.streaming,
            Err(e) => {
                eprintln!("(capabilities query failed: {e})");
                None
            }
        },
    };
    let mode = myna_core::effective_mode(stored, streams);
    (mode.mode, mode.to_string())
}

/// Batch clip mode: run each clip in sequence and exit — no trigger needed.
async fn dictate_clips<B: BackendClient>(backend: B, args: &Args) -> ExitCode {
    let n = args.clips.len();
    let plural = if n == 1 { "" } else { "s" };
    println!("dictating {n} clip{plural} to {}", args.socket.display());

    let (mode, reason) = clip_mode(
        args.mode,
        || myna_core::Settings::load().streaming_mode,
        &args.socket,
    )
    .await;
    println!("mode: {reason}");
    let mut streaming_sink = UnstableFilter {
        inner: StdoutSink,
        show_unstable: args.show_unstable,
    };
    let mut batch_sink = BatchDisplay(UnstableFilter {
        inner: StdoutSink,
        show_unstable: args.show_unstable,
    });
    let mut exit = ExitCode::SUCCESS;

    for clip in &args.clips {
        if let Some(reference) = &clip.reference {
            println!("── clip {}: (reference: {reference})", clip.path.display());
        } else {
            println!("── clip {}", clip.path.display());
        }
        let source = match open_clip(&clip.path) {
            Ok(s) => s.realtime(args.realtime),
            Err(e) => {
                eprintln!("✗ {e}");
                exit = ExitCode::FAILURE;
                continue;
            }
        };
        let config = SessionConfig {
            language: args.language.clone(),
            keywords: args.vocabulary.clone(),
            ..Default::default()
        };
        let outcome = match mode {
            myna_core::StreamingMode::Batch => {
                run_dictation(&backend, config, source, &mut batch_sink).await
            }
            myna_core::StreamingMode::Streaming => {
                run_dictation(&backend, config, source, &mut streaming_sink).await
            }
        };
        match outcome {
            Ok(SessionOutcome::Completed { .. }) => {} // StdoutSink already printed it
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, failure }) => {
                eprintln!("✗ {} [{code}]\n  {}", failure.headline, failure.detail);
                exit = ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("✗ {}\n  could not open session: {e}", e.headline());
                exit = ExitCode::FAILURE;
            }
        }
    }

    exit
}

/// Interactive push-to-talk mic mode: press Enter to speak, Enter to stop, Ctrl-D to quit.
async fn dictate_mic<B: BackendClient>(
    backend: B,
    args: &Args,
    mut lab: Option<&mut run::Lab>,
) -> ExitCode {
    let mut utterance = 0;
    let mut trigger = StdinTrigger::new();

    println!(
        "dictating to {} — press Enter to speak, Enter again to stop, Ctrl-D to quit",
        args.socket.display()
    );

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
        let stamp = run::Stop::default();
        let source = run::Stamped::new(source, stamp.clone());
        let config = SessionConfig {
            language: args.language.clone(),
            keywords: args.vocabulary.clone(),
            ..Default::default()
        };

        // Start the VU meter: runs as a background task, writes \r bars,
        // and clears the line when the stats sender drops (session ends).
        let meter = tokio::spawn(render_meter(mic_stats.clone()));
        let printer = if lab.is_some() {
            Printer::Quiet(run::Quiet)
        } else {
            Printer::Meter(MicMeterSink(StdoutSink))
        };
        let mut sink = run::Recording::new(UnstableFilter {
            inner: printer,
            show_unstable: args.show_unstable,
        });

        // Run the utterance while watching for a Release edge (graceful early
        // stop → end-of-audio → finalize).
        let mut quit = false;
        let outcome = {
            let fut = run_dictation(&backend, config, source, &mut sink);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    result = &mut fut => break result,
                    edge = trigger.next_edge() => match edge {
                        Some(TriggerEdge::Release) => stop.stop(),
                        Some(TriggerEdge::Press) => {} // ignore an extra press
                        None => { stop.stop(); quit = true; }
                    }
                }
            }
        };

        // Stop the meter and ensure the line is clear before any outcome text.
        meter.abort();
        let _ = meter.await;
        print!("\r\x1b[2K");
        let _ = std::io::stdout().flush();

        match outcome {
            Ok(SessionOutcome::Completed { .. }) => {
                // StdoutSink already printed it, unless the lab is timing it.
                if let Some(lab) = lab.as_deref_mut() {
                    utterance += 1;
                    let speech = lab::Speech::recorded(
                        format!("mic {utterance}"),
                        None,
                        &sink.events,
                        stamp.at(),
                    );
                    lab.analyse(speech).await;
                }
            }
            Ok(SessionOutcome::Aborted) => println!("  (aborted)"),
            Ok(SessionOutcome::Failed { code, failure }) if code == "capture_failed" => {
                eprintln!("✗ {}\n  {}", failure.headline, failure.detail);
                eprintln!("  (no mic? check `wpctl status` lists an Audio Source, or pass --target <node.name>)");
            }
            Ok(SessionOutcome::Failed { code, failure }) => {
                eprintln!("✗ {} [{code}]\n  {}", failure.headline, failure.detail);
            }
            Err(e) => eprintln!("✗ {}\n  could not open session: {e}", e.headline()),
        }

        // The T51 acceptance readout: how much the mic captured.
        let s = *mic_stats.borrow();
        println!("  (mic: captured {:.1?})", s.captured);
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

#[cfg(test)]
mod tests {
    use super::*;
    use myna_orchestrator::AudioSource as _;

    #[test]
    fn a_clip_is_converted_to_the_backend_format() {
        let (rate, channels, frames) = (48_000u32, 2u16, 4_800u32);
        let data_len = frames * u32::from(channels) * 2;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&channels.to_le_bytes());
        wav.extend_from_slice(&rate.to_le_bytes());
        wav.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        wav.extend_from_slice(&(channels * 2).to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.resize(wav.len() + data_len as usize, 0);
        let path = std::env::temp_dir().join(format!("myna-clip-{}.wav", std::process::id()));
        std::fs::write(&path, wav).unwrap();

        let source = open_clip(&path);
        std::fs::remove_file(&path).ok();
        let source = source.unwrap();
        assert_eq!(source.format(), AudioFormat::default());
        assert_eq!(source.pcm().len(), 1_600 * 2, "0.1 s at 16 kHz mono S16");
    }

    #[test]
    fn a_missing_clip_is_named() {
        let err = open_clip(std::path::Path::new("/nonexistent/clip.wav"))
            .err()
            .unwrap();
        assert!(
            err.starts_with("cannot open /nonexistent/clip.wav: "),
            "{err}"
        );
    }

    fn parse(flags: &[&str]) -> Result<Args, String> {
        let fixed = ["--socket", "/s", "--clip", "c.wav", "--mode", "batch"];
        parse_args(fixed.iter().chain(flags).map(|s| s.to_string()))
    }

    #[tokio::test]
    async fn the_clip_mode_names_where_it_came_from() {
        use myna_core::StreamingMode::{Batch, Streaming};
        let absent = std::path::Path::new("/nonexistent/backend.sock");
        let (mode, reason) = clip_mode(Some(Batch), || Some(Streaming), absent).await;
        assert_eq!(
            (mode, reason.as_str()),
            (Batch, "streaming-mode Batch, from --mode")
        );
        let (mode, reason) = clip_mode(None, || Some(Batch), absent).await;
        assert_eq!(
            (mode, reason.as_str()),
            (Batch, "streaming-mode Batch, set by the user")
        );
        let (_, reason) = clip_mode(None, || None, absent).await;
        assert_eq!(reason, myna_core::effective_mode(None, None).to_string());
    }

    #[test]
    fn graphs_are_parsed_with_the_exact_baseline_first() {
        let lab = parse(&["--graph", "tidy", "--field", "email", "--mid-sentence"])
            .unwrap()
            .lab
            .unwrap();
        let labels: Vec<_> = lab.specs.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["exact", "tidy"]);
        assert!(lab.mid_sentence);
        assert_eq!(lab.purpose, myna_desktop::inject::FieldPurpose::Email);
        let sweep = parse(&["--sweep", "disfluency,cleanup"])
            .unwrap()
            .lab
            .unwrap();
        assert_eq!(sweep.specs.len(), 1 + 4);
        let only_exact = parse(&["--graph", "exact"]).unwrap().lab.unwrap();
        assert_eq!(only_exact.specs.len(), 1);
    }

    #[test]
    fn without_graph_flags_nothing_changes() {
        assert!(parse(&[]).unwrap().lab.is_none());
        assert!(parse(&["--json", "x"]).err().unwrap().contains("--graph"));
        assert!(parse(&["--graph", "shout"])
            .err()
            .unwrap()
            .contains("shout"));
        assert!(parse(&["--field", "fax", "--graph", "tidy"]).is_err());
    }

    #[test]
    fn text_needs_no_speech_backend() {
        let args = parse_args(["--text", "um hello", "--text", "bye"].map(String::from)).unwrap();
        assert!(args.socket.as_os_str().is_empty());
        assert_eq!(args.lab.unwrap().text, ["um hello", "bye"]);
        assert!(parse(&["--text", "x"])
            .err()
            .unwrap()
            .contains("alternative"));
    }

    #[test]
    fn corpus_clips_can_be_limited() {
        assert!(parse(&["--only", "nothing-matches"])
            .err()
            .unwrap()
            .contains("no clips"));
    }

    #[test]
    fn clips_are_sent_flat_out_unless_realtime_is_asked_for() {
        assert!(!parse(&[]).unwrap().realtime);
        assert!(parse(&["--realtime"]).unwrap().realtime);
    }

    #[test]
    fn a_failed_search_names_the_directory() {
        let dir = PathBuf::from("/nonexistent/share");
        let err = resolve_socket(&BackendSocket::Search(dir)).err().unwrap();
        let (headline, detail) = err.split_once('\n').expect("two lines");
        assert_eq!(headline, "Model not connected");
        assert!(
            detail == "/nonexistent/share: no model is connected",
            "{err}"
        );
    }
}
