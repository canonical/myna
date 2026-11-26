//! What is done to a piece of text, as data: operations run in order, each
//! reading what the one before made. A chain is a straight line, so the list
//! is the whole graph. Every operation fails open, handing on what it got.

use myna_core::ChineseScript;

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
        self.leaves()
            .filter_map(|op| match op {
                Op::Ask { request, .. } => Some(request),
                _ => None,
            })
            .filter(|r| seen.insert(r.key()))
            .cloned()
            .collect()
    }

    /// Whether any operation converts a script, which loads the dictionaries.
    pub fn converts(&self) -> bool {
        self.leaves().any(|op| matches!(op, Op::Convert(_)))
    }

    fn leaves(&self) -> impl Iterator<Item = &Op> {
        self.0.iter().flat_map(|op| match op {
            Op::Chain(ops) => ops.as_slice(),
            one => std::slice::from_ref(one),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A change that needs no model.
    Rule { rule: Rule },
    /// A change a model makes, typed only if it passes `check`.
    Ask { request: Request, check: Check },
    /// Chinese written in another script, by OpenCC.
    Convert(ChineseScript),
    /// Each reads what the one before made; unless all pass, the text
    /// leaves as it came. Holds only the operations above.
    Chain(Vec<Op>),
}
