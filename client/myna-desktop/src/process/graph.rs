//! What is done to a piece of text, as data: operations run in order, each
//! reading what the one before made. A chain is a straight line, so the list
//! is the whole graph. Every operation fails open, handing on what it got.

use super::check::Check;
use super::request::Request;
use super::rules::Rule;

/// The operations a dictation runs, in order; none types the text as heard.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph(Vec<Op>);

impl Graph {
    pub fn passthrough() -> Self {
        Self::default()
    }

    pub fn of(ops: Vec<Op>) -> Self {
        Self(ops)
    }

    pub fn ops(&self) -> &[Op] {
        &self.0
    }

    /// Whether any operation changes the text.
    pub fn touches(&self) -> bool {
        !self.0.is_empty()
    }

    /// Every distinct request any operation makes, in order.
    pub fn requests(&self) -> Vec<Request> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for op in &self.0 {
            let asks = match op {
                Op::Ask { request, .. } => vec![request],
                Op::Pivot { first, second, .. } => vec![first, second],
                Op::Rule { .. } => Vec::new(),
            };
            out.extend(asks.into_iter().filter(|r| seen.insert(r.key())).cloned());
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A change that needs no model.
    Rule { rule: Rule },
    /// A change a model makes, typed only if it passes `check`.
    Ask { request: Request, check: Check },
    /// The second leg reads what the first made; unless both pass, the text
    /// leaves as it came.
    Pivot {
        first: Request,
        second: Request,
        check: Check,
    },
}
