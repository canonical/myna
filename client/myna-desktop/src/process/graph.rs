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

use std::collections::HashMap;

use myna_core::ProcessingChain;
use myna_platform::text_input::FieldPurpose;

use super::pipeline::{Check, Request};
use super::rules::Rule;
use super::steps::{compile, preset_steps, Context};
use super::Field;

/// How much of a dictation a node has to see at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Needs {
    /// A finished sentence is enough.
    Sentence,
    /// Everything that was said.
    Dictation,
}

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

/// What a [`Op::Route`] case asks of the field the text is going to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    Purpose(&'static [FieldPurpose]),
    MidSentence,
}

impl When {
    fn holds(self, field: &Field) -> bool {
        match self {
            When::Purpose(purposes) => purposes.contains(&field.purpose),
            When::MidSentence => field.mid_sentence,
        }
    }
}

/// How an [`Op::Join`] lays its parts out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// One after another, `sep` between them.
    Sep(&'static str),
    /// The first, then the others in brackets: "Hello. (Bonjour.)".
    Gloss,
}

impl Form {
    /// `parts` laid out. An empty part, or one the same as the part kept
    /// before it, adds nothing: a branch that failed open hands on what it
    /// was given, and "Hello. (Hello.)" says nothing twice.
    pub fn apply(self, parts: Vec<String>) -> String {
        let mut kept: Vec<String> = Vec::with_capacity(parts.len());
        for part in parts {
            let part = part.trim();
            if !part.is_empty() && kept.last().is_none_or(|last| last != part) {
                kept.push(part.to_owned());
            }
        }
        match self {
            Form::Sep(sep) => kept.join(sep),
            Form::Gloss => match kept.split_first() {
                Some((first, rest)) if !rest.is_empty() => {
                    format!("{first} ({})", rest.join(" "))
                }
                _ => kept.concat(),
            },
        }
    }
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
        needs: Needs,
    },
    /// The first case whose condition holds for the field picks the source;
    /// the others do not run.
    Route {
        cases: Vec<(When, Src)>,
        otherwise: Src,
    },
    /// Every part, in order, once each has arrived for the piece.
    Join { parts: Vec<Src>, form: Form },
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
            Op::Route { cases, otherwise } => cases
                .iter()
                .map(|(_, src)| *src)
                .chain([*otherwise])
                .collect(),
            Op::Join { parts, .. } => parts.clone(),
        }
    }

    fn needs(&self) -> Needs {
        match self {
            Op::Ask { needs, .. } => *needs,
            Op::Rule { .. } | Op::Route { .. } | Op::Join { .. } | Op::Pivot { .. } => {
                Needs::Sentence
            }
        }
    }

    /// The requests this node makes, in the order it makes them.
    fn asks(&self) -> Vec<&Request> {
        match self {
            Op::Ask { request, .. } => vec![request],
            Op::Pivot { first, second, .. } => vec![&first.request, &second.request],
            Op::Rule { .. } | Op::Route { .. } | Op::Join { .. } => Vec::new(),
        }
    }

    /// The source a route picks for `field`.
    pub fn select(&self, field: &Field) -> Option<Src> {
        match self {
            Op::Route { cases, otherwise } => Some(
                cases
                    .iter()
                    .find(|(when, _)| when.holds(field))
                    .map_or(*otherwise, |(_, src)| *src),
            ),
            _ => None,
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

    /// The most any node reachable from the output has to see at once, over
    /// every branch a route might take; `None` when text passes untouched.
    pub fn needs(&self) -> Option<Needs> {
        self.reachable(|op| op.sources())
            .into_iter()
            .map(|id| self.nodes[id.0].needs())
            .max()
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

    /// The nodes to run for a piece going to `field`, in order: those the
    /// output depends on, with each route reduced to the source it picks.
    pub fn plan(&self, field: &Field) -> Vec<Id> {
        let mut ids = self.reachable(|op| match op.select(field) {
            Some(src) => vec![src],
            None => op.sources(),
        });
        ids.sort_unstable();
        ids
    }

    fn reachable(&self, next: impl Fn(&Op) -> Vec<Src>) -> Vec<Id> {
        let mut seen = vec![false; self.nodes.len()];
        let mut todo = vec![self.out];
        while let Some(src) = todo.pop() {
            if let Src::Node(id) = src {
                if !std::mem::replace(&mut seen[id.0], true) {
                    todo.extend(next(&self.nodes[id.0]));
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

    pub fn ask(&mut self, from: Src, request: Request, check: Check, needs: Needs) -> Src {
        self.push(Op::Ask {
            from,
            request,
            check,
            needs,
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
        self.ask(
            from,
            Request::cleanup(None),
            Check::ConservativeEdit,
            Needs::Sentence,
        )
    }

    pub fn route(&mut self, cases: Vec<(When, Src)>, otherwise: Src) -> Src {
        self.push(Op::Route { cases, otherwise })
    }

    /// `parts` are distinct: a part is told apart by its source.
    pub fn join(&mut self, parts: Vec<Src>, form: Form) -> Src {
        assert!(
            parts
                .iter()
                .enumerate()
                .all(|(i, part)| !parts[..i].contains(part)),
            "a join names each source once"
        );
        self.push(Op::Join { parts, form })
    }

    pub fn finish(self, out: Src) -> Graph {
        Graph {
            nodes: self.nodes,
            out,
        }
    }
}

/// What the joins of a graph hold while a piece's other parts are on their
/// way, keyed by piece, join and the part that arrived, so a late part of
/// one piece can never complete another.
#[derive(Default)]
pub struct Joins {
    pieces: u64,
    held: HashMap<(u64, Id, Src), String>,
}

impl Joins {
    /// The number of the next piece, which no earlier one had.
    pub fn begin(&mut self) -> u64 {
        self.pieces += 1;
        self.pieces
    }

    /// A part of `join` arrived for piece `seq`. The whole once every part
    /// of `parts` has arrived, which also frees what was held for it.
    pub fn arrive(
        &mut self,
        seq: u64,
        join: Id,
        parts: &[Src],
        from: Src,
        text: String,
    ) -> Option<Vec<String>> {
        self.held.insert((seq, join, from), text);
        if !parts
            .iter()
            .all(|p| self.held.contains_key(&(seq, join, *p)))
        {
            return None;
        }
        parts
            .iter()
            .map(|p| self.held.remove(&(seq, join, *p)))
            .collect()
    }

    /// Nothing is held for pieces up to `seq`.
    pub fn forget(&mut self, seq: u64) {
        self.held.retain(|(held, _, _), _| *held > seq);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.held.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translate(g: &mut Builder, from: Src, needs: Needs) -> Src {
        g.ask(from, Request::translate("en", "fr"), Check::Present, needs)
    }

    fn side_by_side(needs: Needs) -> Graph {
        let mut g = Builder::default();
        let french = translate(&mut g, Src::Input, needs);
        let out = g.join(vec![Src::Input, french], Form::Sep(" / "));
        g.finish(out)
    }

    fn tasks(graph: &Graph, field: &Field) -> Vec<String> {
        graph
            .plan(field)
            .into_iter()
            .map(|id| match graph.node(id) {
                Op::Rule { rule, .. } => format!("{rule:?}"),
                Op::Ask { request, .. } => request.task.clone(),
                Op::Route { .. } => "route".into(),
                Op::Join { .. } => "join".into(),
                Op::Pivot { first, second, .. } => {
                    format!("{}+{}", first.request.task, second.request.task)
                }
            })
            .collect()
    }

    #[test]
    fn presets_are_graphs_of_what_they_do() {
        let field = Field::default();
        let exact = Graph::preset(ProcessingChain::EXACT);
        assert_eq!(exact, Graph::passthrough());
        assert_eq!(exact.needs(), None);
        assert!(tasks(&exact, &field).is_empty());

        let tidy = Graph::preset(ProcessingChain::TIDY);
        assert_eq!(
            tasks(&tidy, &field),
            ["Disfluency(Any)", "Vocabulary", "text.cleanup"]
        );
        assert_eq!(tidy.needs(), Some(Needs::Sentence));

        let french = Graph::preset(ProcessingChain::TIDY_FRENCH);
        assert_eq!(
            tasks(&french, &field),
            [
                "Disfluency(Any)",
                "Vocabulary",
                "text.cleanup",
                "text.translate"
            ]
        );
        assert_eq!(french.needs(), Some(Needs::Sentence));
    }

    #[test]
    fn every_preset_has_a_graph_and_a_unique_one() {
        // `keyword` was fitting alone, which is no graph now (`process::fit`).
        let graphs: Vec<_> = ProcessingChain::ALL
            .into_iter()
            .filter(|chain| *chain != ProcessingChain::KEYWORD)
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
    fn a_pivot_asks_both_legs_and_needs_a_sentence() {
        let mut g = Builder::default();
        let out = g.pivot(
            Src::Input,
            Request::translate("fr", "en"),
            Request::translate("en", "de"),
            Check::Present,
        );
        let graph = g.finish(out);
        assert_eq!(graph.needs(), Some(Needs::Sentence));
        assert_eq!(
            graph.requests(),
            [
                Request::translate("fr", "en"),
                Request::translate("en", "de")
            ]
        );
        assert_eq!(
            tasks(&graph, &Field::default()),
            ["text.translate+text.translate"]
        );
    }

    #[test]
    fn a_gloss_puts_the_rest_in_brackets_after_the_first() {
        let parts = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            Form::Gloss.apply(parts(&["Hello.", "Bonjour."])),
            "Hello. (Bonjour.)"
        );
        assert_eq!(Form::Sep(" / ").apply(parts(&["a", "b", "c"])), "a / b / c");
    }

    #[test]
    fn a_part_the_same_as_the_one_before_adds_nothing() {
        let parts = |p: &[&str]| p.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Form::Gloss.apply(parts(&["Hello.", " Hello. "])), "Hello.");
        assert_eq!(
            Form::Sep(" / ").apply(parts(&["a", "a", "b", "a"])),
            "a / b / a"
        );
        assert_eq!(Form::Gloss.apply(parts(&["", "Bonjour."])), "Bonjour.");
    }

    #[test]
    fn a_join_of_sentence_branches_streams() {
        assert_eq!(side_by_side(Needs::Sentence).needs(), Some(Needs::Sentence));
    }

    #[test]
    fn one_branch_that_needs_the_dictation_holds_the_whole_of_it() {
        assert_eq!(
            side_by_side(Needs::Dictation).needs(),
            Some(Needs::Dictation)
        );
        let mut g = Builder::default();
        let whole = translate(&mut g, Src::Input, Needs::Dictation);
        let tidy = g.tidy(whole);
        assert_eq!(g.finish(tidy).needs(), Some(Needs::Dictation));
    }

    #[test]
    fn a_node_nothing_reads_does_not_hold_the_dictation() {
        let mut g = Builder::default();
        translate(&mut g, Src::Input, Needs::Dictation);
        let tidy = g.tidy(Src::Input);
        let graph = g.finish(tidy);
        assert_eq!(graph.needs(), Some(Needs::Sentence));
        assert_eq!(tasks(&graph, &Field::default()), ["text.cleanup"]);
    }

    #[test]
    fn a_route_needs_what_any_of_its_branches_do() {
        let mut g = Builder::default();
        let whole = translate(&mut g, Src::Input, Needs::Dictation);
        let out = g.route(vec![(When::MidSentence, whole)], Src::Input);
        assert_eq!(g.finish(out).needs(), Some(Needs::Dictation));
    }

    #[test]
    fn a_route_runs_only_the_branch_the_field_picks() {
        let mut g = Builder::default();
        let address = g.rule(Src::Input, Rule::Address);
        let go_on = g.rule(Src::Input, Rule::Continue);
        let go_on = g.rule(go_on, Rule::Vocabulary);
        let heard = g.rule(Src::Input, Rule::Vocabulary);
        let out = g.route(
            vec![
                (
                    When::Purpose(&[FieldPurpose::Url, FieldPurpose::Email]),
                    address,
                ),
                (When::MidSentence, go_on),
            ],
            heard,
        );
        let keyword = g.finish(out);
        let after = |purpose, before| Field::after(purpose, before);
        assert_eq!(
            tasks(&keyword, &after(FieldPurpose::Email, None)),
            ["Address", "route"]
        );
        assert_eq!(
            tasks(&keyword, &after(FieldPurpose::Url, Some('x'))),
            ["Address", "route"]
        );
        assert_eq!(
            tasks(&keyword, &after(FieldPurpose::Plain, Some('x'))),
            ["Continue", "Vocabulary", "route"]
        );
        assert_eq!(
            tasks(&keyword, &after(FieldPurpose::Plain, Some('.'))),
            ["Vocabulary", "route"]
        );
        assert_eq!(
            tasks(&keyword, &after(FieldPurpose::Plain, None)),
            ["Vocabulary", "route"]
        );
        assert_eq!(keyword.needs(), Some(Needs::Sentence));
    }

    #[test]
    fn a_field_is_mid_sentence_after_a_word_or_a_clause_mark() {
        let mid = |c| Field::after(FieldPurpose::Plain, c).mid_sentence;
        assert!(mid(Some('a')) && mid(Some('7')) && mid(Some(',')) && mid(Some(':')));
        assert!(!mid(None) && !mid(Some(' ')) && !mid(Some('.')) && !mid(Some('\n')));
    }

    #[test]
    fn a_join_waits_for_every_part_of_its_own_piece() {
        let mut g = Builder::default();
        let french = translate(&mut g, Src::Input, Needs::Sentence);
        let Src::Node(join) = g.join(vec![Src::Input, french], Form::Sep(" / ")) else {
            unreachable!()
        };
        let parts = [Src::Input, french];
        let mut joins = Joins::default();
        assert_eq!([joins.begin(), joins.begin()], [1, 2]);
        let arrive = |joins: &mut Joins, seq, from, text: &str| {
            joins.arrive(seq, join, &parts, from, text.into())
        };

        assert_eq!(arrive(&mut joins, 1, french, "Un."), None);
        assert_eq!(arrive(&mut joins, 2, french, "Deux."), None);
        assert_eq!(
            arrive(&mut joins, 2, Src::Input, "Two."),
            Some(vec!["Two.".into(), "Deux.".into()])
        );
        assert_eq!(
            arrive(&mut joins, 1, Src::Input, "One."),
            Some(vec!["One.".into(), "Un.".into()])
        );
        assert_eq!(joins.len(), 0);

        arrive(&mut joins, 3, Src::Input, "Three.");
        arrive(&mut joins, 4, Src::Input, "Four.");
        joins.forget(3);
        assert_eq!(joins.len(), 1);
        assert_eq!(arrive(&mut joins, 3, french, "Trois."), None);
        assert_eq!(
            arrive(&mut joins, 4, french, "Quatre."),
            Some(vec!["Four.".into(), "Quatre.".into()])
        );
    }

    #[test]
    #[should_panic(expected = "each source once")]
    fn a_join_cannot_name_a_source_twice() {
        Builder::default().join(vec![Src::Input, Src::Input], Form::Sep(" "));
    }
}
