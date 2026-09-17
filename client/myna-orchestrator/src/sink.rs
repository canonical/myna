//! The text-output boundary (plan T41, stands in for T22) — a [`TextSink`]
//! consumes the orchestrator's [`OrchestratorEvent`]s and renders them. The real
//! IBus injector (T22) implements the same trait; the demo mock prints to
//! stdout, and a collecting mock captures events for tests.

use async_trait::async_trait;

use crate::fsm::{DropReason, OrchestratorEvent};

/// Where committed text and lifecycle events go. Commit-only for the MVP — only
/// `Final`/`Done` text is ever inserted; `Snippet` is unstable UI and is never
/// committed (the "no retraction" invariant).
#[async_trait]
pub trait TextSink: Send {
    async fn emit(&mut self, event: OrchestratorEvent);
}

/// Where a [`RenderedLine`] goes (feature 011-accessible-dictation-ux, US5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// One line of plain, line-oriented text plus which stream it belongs on
/// (feature 011-accessible-dictation-ux, US5, FR-028/029, contracts T1-T3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedLine {
    pub stream: OutputStream,
    pub text: String,
}

impl RenderedLine {
    fn stdout(text: impl Into<String>) -> Self {
        Self {
            stream: OutputStream::Stdout,
            text: text.into(),
        }
    }

    fn stderr(text: impl Into<String>) -> Self {
        Self {
            stream: OutputStream::Stderr,
            text: text.into(),
        }
    }
}

/// Render one [`OrchestratorEvent`] as a single, line-oriented text line, or
/// `None` when the event produces no output (`Transcribing` is an internal
/// liveness ping only).
///
/// Feature 011-accessible-dictation-ux (US5, FR-028, contract T1): every
/// line carries an explicit `[marker]` textual tag as its *primary* content
/// — the emoji are a decorative addition, never the only signal, so meaning
/// survives with colour/emoji stripped (there is no colour here today; the
/// requirement is about never regressing to emoji-only). (FR-029, contract
/// T3): a mid-stream `Error` routes through the shared `myna_core::failure`
/// registry (`lookup_by_code`) — the identical presentation
/// `SessionOutcome::Failed`/`BackendError` resolve to elsewhere in
/// `myna-cli`'s `main.rs` (contract F2/F3), never a separate ad-hoc
/// CLI-only string. This function is pure (no I/O), so it's hermetically
/// testable without capturing real stdout — [`StdoutSink::emit`] is the thin
/// I/O wrapper that prints what this returns.
pub fn render_event_line(event: &OrchestratorEvent) -> Option<RenderedLine> {
    match event {
        OrchestratorEvent::Loading => Some(RenderedLine::stdout("[loading] ⏳ loading model…")),
        OrchestratorEvent::Ready => Some(RenderedLine::stdout("[ready] 🎤 ready — listening")),
        OrchestratorEvent::Transcribing => None,
        OrchestratorEvent::Snippet(text) => {
            Some(RenderedLine::stdout(format!("[progress] … {text}")))
        }
        OrchestratorEvent::Final(text) => {
            Some(RenderedLine::stdout(format!("[committed] » {text}")))
        }
        OrchestratorEvent::Unstable(text) => {
            Some(RenderedLine::stdout(format!("[partial] ~ {text}")))
        }
        OrchestratorEvent::Done(text) if text.trim().is_empty() => {
            Some(RenderedLine::stdout("[done] ✓ (no speech detected)"))
        }
        OrchestratorEvent::Done(text) => Some(RenderedLine::stdout(format!("[done] ✓ {text}"))),
        OrchestratorEvent::Error { code, message } => {
            let presentation = myna_core::failure::lookup_by_code(code);
            Some(RenderedLine::stderr(format!(
                "[error] {}",
                presentation.render(Some(message))
            )))
        }
        OrchestratorEvent::AudioDropped(reason) => {
            let why = match reason {
                DropReason::NotResident => "model not ready",
                DropReason::NotActive => "session not accepting audio",
            };
            Some(RenderedLine::stderr(format!(
                "[dropped-audio] (dropped audio: {why})"
            )))
        }
    }
}

/// Prints a human-readable dictation session to stdout, mirroring the feedback
/// in `dev/dictate.py` (loading indicator, committed segments, final line,
/// errors). A thin I/O wrapper over the pure [`render_event_line`] — always
/// prints via `println!`/`eprintln!`, which always terminate in `\n` and
/// never emit a bare `\r`/ANSI cursor-movement sequence (FR-029, contract T2).
#[derive(Default)]
pub struct StdoutSink;

#[async_trait]
impl TextSink for StdoutSink {
    async fn emit(&mut self, event: OrchestratorEvent) {
        let Some(line) = render_event_line(&event) else {
            return;
        };
        match line.stream {
            OutputStream::Stdout => println!("{}", line.text),
            OutputStream::Stderr => eprintln!("{}", line.text),
        }
    }
}

/// Captures every event for assertions in tests.
#[derive(Default)]
pub struct CollectingSink {
    pub events: Vec<OrchestratorEvent>,
}

impl CollectingSink {
    /// The committed segment texts, in order (`Final` events).
    pub fn finals(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match e {
                OrchestratorEvent::Final(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    }

    /// The full transcript, if the session completed (`Done`).
    pub fn done(&self) -> Option<String> {
        self.events.iter().rev().find_map(|e| match e {
            OrchestratorEvent::Done(t) => Some(t.clone()),
            _ => None,
        })
    }
}

#[async_trait]
impl TextSink for CollectingSink {
    async fn emit(&mut self, event: OrchestratorEvent) {
        self.events.push(event);
    }
}
