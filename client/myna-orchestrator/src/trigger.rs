//! The hotkey boundary (plan T41, stands in for T21) — a [`Trigger`] yields
//! press/release edges that bound a push-to-talk utterance. The real
//! `org.freedesktop.portal.GlobalShortcuts` hotkey (T21) implements the same
//! trait; the demo mock reads lines from stdin.

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader, Stdin};

/// A push-to-talk edge. `Press` starts an utterance; `Release` is the graceful
/// stop (end-of-audio → finalize) — see `docs/audio-adapter-api.md` §5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriggerEdge {
    Press,
    Release,
}

/// A source of push-to-talk edges. `None` ends the trigger (e.g. stdin EOF /
/// the shortcut being unbound) — the demo treats it as "quit".
#[async_trait]
pub trait Trigger: Send {
    async fn next_edge(&mut self) -> Option<TriggerEdge>;

    /// Drop any edges that have queued up since the last `next_edge()` call
    /// *without* delivering them — used by the controller between utterances
    /// to swallow hotkey spam that arrived during finalization (so it doesn't
    /// bounce the session straight back into another Recording→Finalizing
    /// cycle). Default: nothing to drain (hold-to-talk sources emit no ghost
    /// edges; a stray press during a session is a real user intent).
    async fn discard_pending(&mut self) {}

    /// Force this trigger's internal press/release parity back to "not
    /// recording", *without* consuming a real edge — used by the controller
    /// when an utterance ends for a reason other than reading a matching
    /// edge off this trigger (e.g. focus-loss ends the session via
    /// `stop.stop()`, never touching the trigger). Toggle-style triggers
    /// (`ControlTrigger`, `GlobalShortcutTrigger` in `Toggle` mode) track
    /// "pressed" as *session-active*, decoupled from any physical key state;
    /// left unsynced, the next real user poke flips that bit the "wrong" way
    /// and delivers a `Release` (silently swallowed while idle) instead of a
    /// `Press` — the user has to press twice to resume (manual test report,
    /// 2026-07-31). Default: a no-op (hold-to-talk / scripted sources track
    /// real physical/test state that this desync can't touch).
    async fn resync(&mut self) {}
}

/// Reads stdin lines and toggles `Press`/`Release` per line — the two-Enter
/// flow from `dev/dictate.py` (Enter to start, Enter to stop). EOF (`Ctrl-D`)
/// ends the trigger.
pub struct StdinTrigger {
    lines: tokio::io::Lines<BufReader<Stdin>>,
    pressed: bool,
}

impl Default for StdinTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl StdinTrigger {
    pub fn new() -> Self {
        Self { lines: BufReader::new(tokio::io::stdin()).lines(), pressed: false }
    }
}

#[async_trait]
impl Trigger for StdinTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        match self.lines.next_line().await {
            Ok(Some(_line)) => {
                self.pressed = !self.pressed;
                Some(if self.pressed { TriggerEdge::Press } else { TriggerEdge::Release })
            }
            // EOF or a read error both end the trigger.
            Ok(None) | Err(_) => None,
        }
    }
}

/// A pre-scripted trigger for tests: yields a fixed sequence of edges.
pub struct ScriptedTrigger {
    edges: std::collections::VecDeque<TriggerEdge>,
}

impl ScriptedTrigger {
    pub fn new(edges: impl IntoIterator<Item = TriggerEdge>) -> Self {
        Self { edges: edges.into_iter().collect() }
    }
}

#[async_trait]
impl Trigger for ScriptedTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        self.edges.pop_front()
    }
}
