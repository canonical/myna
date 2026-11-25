//! What the resolver publishes about the chain: whether its models are
//! installed, which requests are missing, and the catalog of what every
//! connected share offers. Settings and the pill read these through the
//! daemon's D-Bus properties; nothing here holds dictated text.

use std::collections::BTreeSet;

use super::pipeline::Request;
use super::resolve::Candidate;
use super::steps::StepReport;
use super::wire::Params;

/// Whether the chosen graph can do everything it asks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ChainStatus {
    #[default]
    Ready,
    /// Some step has no model: the steps are skipped, the text is typed.
    Degraded {
        /// Requests no connected share covers, in key order.
        missing: Vec<Missing>,
        /// A share was connected after this process started, so only a
        /// fresh daemon can see it.
        needs_restart: bool,
    },
}

/// A request no connected model covers: what to install, never any text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Missing {
    pub task: String,
    pub params: Params,
}

impl Missing {
    pub fn of(request: &Request) -> Self {
        Self {
            task: request.task.clone(),
            params: request.params.clone(),
        }
    }
}

impl ChainStatus {
    /// The tasks with no model, sorted and without repeats.
    pub fn missing_tasks(&self) -> Vec<String> {
        match self {
            Self::Ready => Vec::new(),
            Self::Degraded { missing, .. } => missing
                .iter()
                .map(|m| m.task.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        }
    }

    /// Whether no model covers `request`.
    pub fn lacks(&self, request: &Request) -> bool {
        matches!(self, Self::Degraded { missing, .. }
            if missing.iter().any(|m| m.task == request.task && m.params == request.params))
    }
}

/// What the daemon publishes for Settings besides the status: every model
/// of every connected share, and what became of each step.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub models: Vec<Candidate>,
    pub steps: Vec<StepReport>,
    /// Every saved chain, with whether its models are installed.
    pub chains: Vec<ChainSummary>,
    /// The active chain's id; empty for none.
    pub active: String,
}

/// A saved chain as Settings and the pill list it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChainSummary {
    pub id: String,
    pub name: String,
    pub steps: usize,
    /// Every model step has an installed model (or the chain has none).
    pub ready: bool,
    /// What it lacks when not ready: the requests no installed model covers.
    pub missing: Vec<Request>,
    /// Steps this build does not know.
    pub unknown: usize,
}
