//! What is done to a piece of text, as data.
//!
//! A graph is nodes that name their sources, so a preset is a value, not a
//! variant of code: adding one adds no branch anywhere. A node's source is
//! the text as heard or an earlier node, so a graph cannot loop, and a
//! source may feed several nodes (fan-out). Text leaves from one node, the
//! graph's `out`.
//!
//! Every step fails open: a model step that is missing, late or fails its
//! check hands on the text it was given, so no edge ever carries nothing.

use super::pipeline::{Check, Request};
use super::rules::Rule;
use super::steps::{compile, preset_steps, Context};
use myna_core::ProcessingChain;

/// A node of one graph. Only the graph's builder makes one, which is what
/// keeps a source ahead of the node that reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(usize);

#[cfg(test)]
impl Id {
    pub fn first() -> Self {
        Id(0)
    }
}

/// Where a node's text comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Src {
    /// The piece as heard.
    Input,
    Node(Id),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// A change that needs no model.
    Rule { from: Src, rule: Rule },
    /// A change a model makes, typed only if it passes `check`.
    Ask {
        from: Src,
        request: Request,
        check: Check,
    },
    /// Two model changes that only make sense together, such as translating
    /// through a third language: the second reads what the first made, and
    /// unless both pass the text leaves as it came.
    Pivot { from: Src, first: Leg, second: Leg },
}

/// One model change of an [`Op::Pivot`], typed only if it passes `check`
/// against what it was given.
#[derive(Clone, Debug, PartialEq)]
pub struct Leg {
    pub request: Request,
    pub check: Check,
}

impl Op {
    fn sources(&self) -> Vec<Src> {
        match self {
            Op::Rule { from, .. } | Op::Ask { from, .. } | Op::Pivot { from, .. } => vec![*from],
        }
    }

    /// The requests this node makes, in the order it makes them.
    fn asks(&self) -> Vec<&Request> {
        match self {
            Op::Ask { request, .. } => vec![request],
            Op::Pivot { first, second, .. } => vec![&first.request, &second.request],
            Op::Rule { .. } => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    nodes: Vec<Op>,
    out: Src,
}

impl Graph {
    /// Text typed as heard.
    pub fn passthrough() -> Self {
        Self {
            nodes: Vec::new(),
            out: Src::Input,
        }
    }

    pub fn node(&self, id: Id) -> &Op {
        &self.nodes[id.0]
    }

    pub fn out(&self) -> Src {
        self.out
    }

    /// Whether any node changes the text; false when it passes untouched.
    pub fn touches(&self) -> bool {
        !self.plan().is_empty()
    }

    /// Every distinct request any node makes, in node order.
    pub fn requests(&self) -> Vec<Request> {
        let mut seen = std::collections::HashSet::new();
        self.nodes
            .iter()
            .flat_map(Op::asks)
            .filter(|r| seen.insert(r.key()))
            .cloned()
            .collect()
    }

    /// The nodes to run for a piece, in order: those the output depends on.
    pub fn plan(&self) -> Vec<Id> {
        let mut seen = vec![false; self.nodes.len()];
        let mut todo = vec![self.out];
        while let Some(src) = todo.pop() {
            if let Src::Node(id) = src {
                if !std::mem::replace(&mut seen[id.0], true) {
                    todo.extend(self.nodes[id.0].sources());
                }
            }
        }
        (0..self.nodes.len()).filter(|&i| seen[i]).map(Id).collect()
    }

    /// The graph a preset names: its steps compiled for unknown language.
    pub fn preset(chain: ProcessingChain) -> Self {
        compile(&preset_steps(chain), &Context::default()).graph
    }
}

/// Makes a [`Graph`] a node at a time. A node can name only sources already
/// made.
#[derive(Default)]
pub struct Builder {
    nodes: Vec<Op>,
}

impl Builder {
    fn push(&mut self, op: Op) -> Src {
        self.nodes.push(op);
        Src::Node(Id(self.nodes.len() - 1))
    }

    pub fn rule(&mut self, from: Src, rule: Rule) -> Src {
        self.push(Op::Rule { from, rule })
    }

    pub fn ask(&mut self, from: Src, request: Request, check: Check) -> Src {
        self.push(Op::Ask {
            from,
            request,
            check,
        })
    }

    /// Change the text with `first`, then what that made with `second`.
    pub fn pivot(&mut self, from: Src, first: Request, second: Request, check: Check) -> Src {
        self.push(Op::Pivot {
            from,
            first: Leg {
                request: first,
                check,
            },
            second: Leg {
                request: second,
                check,
            },
        })
    }

    /// Drop hesitations, false starts and repeats.
    pub fn tidy(&mut self, from: Src) -> Src {
        self.ask(from, Request::cleanup(None), Check::ConservativeEdit)
    }

    pub fn finish(self, out: Src) -> Graph {
        Graph {
            nodes: self.nodes,
            out,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tasks(graph: &Graph) -> Vec<String> {
        graph
            .plan()
            .into_iter()
            .map(|id| match graph.node(id) {
                Op::Rule { rule, .. } => format!("{rule:?}"),
                Op::Ask { request, .. } => request.task.clone(),
                Op::Pivot { first, second, .. } => {
                    format!("{}+{}", first.request.task, second.request.task)
                }
            })
            .collect()
    }

    #[test]
    fn presets_are_graphs_of_what_they_do() {
        let exact = Graph::preset(ProcessingChain::EXACT);
        assert_eq!(exact, Graph::passthrough());
        assert!(!exact.touches());

        let tidy = Graph::preset(ProcessingChain::TIDY);
        assert_eq!(
            tasks(&tidy),
            ["Disfluency(Any)", "Vocabulary", "text.cleanup"]
        );
        assert!(tidy.touches());

        let french = Graph::preset(ProcessingChain::TIDY_FRENCH);
        assert_eq!(
            tasks(&french),
            [
                "Disfluency(Any)",
                "Vocabulary",
                "text.cleanup",
                "text.translate"
            ]
        );
    }

    #[test]
    fn every_preset_has_a_graph_and_a_unique_one() {
        let graphs: Vec<_> = ProcessingChain::ALL
            .into_iter()
            .map(Graph::preset)
            .collect();
        for (i, graph) in graphs.iter().enumerate() {
            assert!(
                !graphs[..i].contains(graph),
                "{i} repeats an earlier preset"
            );
        }
    }

    #[test]
    fn a_pivot_asks_both_legs() {
        let mut g = Builder::default();
        let out = g.pivot(
            Src::Input,
            Request::translate("fr", "en"),
            Request::translate("en", "de"),
            Check::Usable { translation: true },
        );
        let graph = g.finish(out);
        assert_eq!(
            graph.requests(),
            [
                Request::translate("fr", "en"),
                Request::translate("en", "de")
            ]
        );
        assert_eq!(tasks(&graph), ["text.translate+text.translate"]);
    }

    #[test]
    fn a_node_nothing_reads_does_not_run() {
        let mut g = Builder::default();
        g.ask(
            Src::Input,
            Request::translate("en", "fr"),
            Check::Usable { translation: true },
        );
        let tidy = g.tidy(Src::Input);
        assert_eq!(tasks(&g.finish(tidy)), ["text.cleanup"]);
    }
}
