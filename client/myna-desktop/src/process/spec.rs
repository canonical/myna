//! A step as stored, what this build knows of it, and what compiling made
//! of it: the vocabulary Settings writes and the daemon reports, kept apart
//! from the compiler that uses it.

use serde_json::Value;

use super::request::Request;
use super::wire::Params;

/// A step as stored: a stable id, its parameters and whether it is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepSpec {
    pub id: String,
    pub params: Params,
    pub on: bool,
}

impl StepSpec {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            params: Params::new(),
            on: true,
        }
    }

    /// A step of a saved chain.
    pub fn of_chain(step: &myna_core::chains::ChainStep) -> Self {
        let spec = step
            .params
            .iter()
            .fold(Self::new(&step.step), |spec, (key, value)| {
                spec.with(key, value)
            });
        Self {
            on: step.on,
            ..spec
        }
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    /// The step `id` with the stored parameters that belong to it
    /// (`translate.to` becomes `to`).
    pub fn stored(id: &str, params: &myna_core::processing::Params) -> Self {
        let prefix = format!("{id}.");
        params
            .iter()
            .filter_map(|(key, value)| Some((key.strip_prefix(&prefix)?, value)))
            .fold(Self::new(id), |step, (key, value)| step.with(key, value))
    }

    /// `id` or `id=value`, the value being the step's one parameter
    /// (`translate=fr`, `script=simplified`).
    pub fn parse(token: &str) -> Self {
        let (id, value) = token.split_once('=').unwrap_or((token, ""));
        let step = Self::new(id.trim());
        match (Kind::of(&step.id).and_then(Kind::param), value.trim()) {
            (Some(key), value) if !value.is_empty() => step.with(key, value),
            _ => step,
        }
    }

    pub(super) fn text(&self, key: &str) -> Option<&str> {
        self.params
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// The steps this build knows, in their default insert order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Drop hesitations, false starts and repeats.
    Tidy,
    /// Translate to `to`.
    Translate,
    /// Write Chinese in `script`.
    Script,
}

impl Kind {
    pub(super) fn of(id: &str) -> Option<Self> {
        [Kind::Tidy, Kind::Translate, Kind::Script]
            .into_iter()
            .find(|k| k.id() == id)
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Tidy => "tidy",
            Kind::Translate => "translate",
            Kind::Script => "script",
        }
    }

    pub(super) fn param(self) -> Option<&'static str> {
        match self {
            Kind::Translate => Some("to"),
            Kind::Script => Some("mode"),
            Kind::Tidy => None,
        }
    }
}

/// What the compiler became of one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepState {
    /// In the graph.
    Applicable,
    /// Left out because the text it would read is not what it reads; not a
    /// fault. `move_above` is the index of the earlier step that changed the
    /// language, when the step would apply above it.
    NotApplicable {
        reason: String,
        move_above: Option<usize>,
    },
    /// Switched off in the chain; left out.
    Off,
    /// In the graph, but no model answers `request`: the step fails open
    /// until one does.
    Unavailable { request: Request },
    /// An id this build does not know; dropped.
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepReport {
    pub id: String,
    pub state: StepState,
}
