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

/// Prints a human-readable dictation session to stdout, mirroring the feedback
/// in `dev/dictate.py` (loading indicator, committed segments, final line,
/// errors).
#[derive(Default)]
pub struct StdoutSink;

#[async_trait]
impl TextSink for StdoutSink {
    async fn emit(&mut self, event: OrchestratorEvent) {
        match event {
            OrchestratorEvent::Loading => println!("⏳ loading model…"),
            OrchestratorEvent::Ready => println!("🎤 ready — listening"),
            OrchestratorEvent::Transcribing => {}
            OrchestratorEvent::Snippet(text) => println!("   … {text}"),
            OrchestratorEvent::Final(text) => println!("   » {text}"),
            OrchestratorEvent::Done(text) if text.trim().is_empty() => {
                println!("✓ (no speech detected)");
            }
            OrchestratorEvent::Done(text) => println!("✓ {text}"),
            OrchestratorEvent::Error { code, message } => {
                eprintln!("✗ [{code}] {message}");
            }
            OrchestratorEvent::AudioDropped(reason) => {
                let why = match reason {
                    DropReason::NotResident => "model not ready",
                    DropReason::NotActive => "session not accepting audio",
                };
                eprintln!("  (dropped audio: {why})");
            }
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
