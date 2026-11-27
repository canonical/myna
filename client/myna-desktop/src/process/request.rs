//! What a step asks of a model: a purpose and its parameters, never a model.
//!
//! The resolver picks the model; the budgets say how long it may take to
//! answer and to load before the step is skipped.

use std::time::Duration;

use super::wire::Params;

/// What a step asks of a model: a purpose, never a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// What to do with the text: `text.cleanup`, `text.translate`.
    pub task: String,
    /// What the task is about: `language`, `to`.
    /// Ordered, so equal requests make equal keys.
    pub params: Params,
    /// How long one answer may take before the step is skipped.
    pub run_budget: Duration,
    /// How long finding and loading a model may take before the next
    /// candidate is tried.
    pub bind_budget: Duration,
}

impl Request {
    /// A request built from data: the task and its parameters, as a step
    /// declares them. The run budget follows the task.
    pub fn of(task: &str, params: Params) -> Self {
        let run_budget = match task {
            "text.cleanup" => Duration::from_secs(1),
            _ => Duration::from_secs(10),
        };
        Self {
            task: task.into(),
            params,
            run_budget,
            bind_budget: BIND_BUDGET,
        }
    }

    pub fn cleanup(language: Option<String>) -> Self {
        let params = language
            .map(|l| ("language".to_owned(), l.into()))
            .into_iter()
            .collect();
        Self::of("text.cleanup", params)
    }

    /// A dictation typed at once is translated at once, so the budget is a
    /// long dictation's: a sentence takes a model 60-260 ms.
    pub fn translate(from: &str, to: &str) -> Self {
        let params = [("language", from), ("to", to)]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.into()))
            .collect();
        Self::of("text.translate", params)
    }

    /// What a binding is cached under: everything that picks a model and
    /// nothing that does not (the run budget).
    pub fn key(&self) -> String {
        let params = serde_json::to_string(&self.params).unwrap_or_default();
        format!("{}|{params}", self.task)
    }
}

/// A model that takes longer to load than this is not the one to dictate
/// with: the next candidate is tried.
pub const BIND_BUDGET: Duration = Duration::from_secs(5);
