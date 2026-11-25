//! Processing as an ordered list of steps, compiled to a [`Graph`].
//!
//! The chain runs in the order the user gave it. A step declares the
//! language it reads and the language it leaves, and one whose input does
//! not match the text at that point is skipped statically: not applicable is
//! no fault, and when moving it above the step that changed the language
//! would fix it, the report says which step that is.

use myna_core::{ChineseScript, ProcessingChain};
use serde_json::Value;

use super::disfluency::Lang;
use super::graph::{Graph, Op};
use super::pipeline::{Check, Request};
use super::rules::Rule;

use super::spec::{misfit, Kind};
pub use super::spec::{StepReport, StepSpec, StepState};

/// What the compiler needs to know besides the steps.
pub struct Context<'a> {
    /// The speech model's language; `None` when not known.
    pub language: Option<&'a str>,
    /// Whether no model answers a request, as far as is known.
    pub unavailable: &'a dyn Fn(&Request) -> bool,
    /// Whether no installed model covers a request, bound or not. Unlike
    /// `unavailable` it does not move with what the graph asks for, so a
    /// translation can pivot on it without flapping.
    pub uncovered: &'a dyn Fn(&Request) -> bool,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            language: None,
            unavailable: &|_| false,
            uncovered: &|_| false,
        }
    }
}

/// A compiled step list.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub graph: Graph,
    /// How Chinese is written after the graph.
    pub script: ChineseScript,
    /// One entry per step, at the step's index in the chain.
    pub report: Vec<StepReport>,
}

impl Compiled {
    /// Whether a dictation must be held back: the graph changes text, or the
    /// script step converts it, and conversion reads a sentence at a time.
    pub fn holds(&self, language: Option<&str>) -> bool {
        self.graph.touches()
            || (self.script != ChineseScript::AsRecognised
                && myna_core::chains::may_be_chinese(language))
    }
}

/// The steps a preset is.
pub fn preset_steps(chain: ProcessingChain) -> Vec<StepSpec> {
    stored_steps(&chain.steps(), &chain.params())
}

/// The steps a settings list and its parameters amount to, in the order
/// stored. Ids this build does not know stay, and compile as unknown.
pub fn stored_steps(ids: &[String], params: &myna_core::processing::Params) -> Vec<StepSpec> {
    ids.iter().map(|id| StepSpec::stored(id, params)).collect()
}

/// The steps as a line for `--status`: `tidy, translate=fr`.
pub fn describe(steps: &[StepSpec]) -> String {
    if steps.is_empty() {
        return "(none)".into();
    }
    let one = |step: &StepSpec| {
        let named = match step.params.values().find_map(Value::as_str) {
            Some(value) => format!("{}={value}", step.id),
            None => step.id.clone(),
        };
        if step.on {
            named
        } else {
            format!("{named} (off)")
        }
    };
    steps.iter().map(one).collect::<Vec<_>>().join(", ")
}

/// A short name for a step list, as the history records it: the preset's
/// nick when the steps are one, else the ids joined by `+`.
pub fn label(steps: &[StepSpec]) -> String {
    let compiled: Vec<&str> = steps.iter().map(|s| s.id.as_str()).collect();
    let is_preset = |chain: ProcessingChain| {
        let ids = chain.steps();
        ids.len() == compiled.len()
            && ids.iter().all(|id| compiled.contains(&id.as_str()))
            && (chain != ProcessingChain::TIDY_FRENCH
                || steps
                    .iter()
                    .any(|s| s.id == "translate" && s.text("to") == Some("fr")))
    };
    match ProcessingChain::ALL
        .into_iter()
        .find(|chain| is_preset(*chain))
    {
        Some(chain) => chain.nick().to_owned(),
        None => compiled.join("+"),
    }
}

/// Compile `steps` in their order for dictation in `ctx.language`.
pub fn compile(steps: &[StepSpec], ctx: &Context) -> Compiled {
    let mut compiler = Compiler {
        ops: Vec::new(),
        lang: ctx.language.map(myna_core::chains::primary_language),
        chinese: ChineseScript::AsRecognised,
        changed: None,
        ctx,
    };
    let report = steps
        .iter()
        .enumerate()
        .map(|(index, step)| StepReport {
            id: step.id.clone(),
            state: compiler.step(index, step),
        })
        .collect();
    Compiled {
        graph: Graph::of(compiler.ops),
        script: compiler.chinese,
        report,
    }
}

/// One compile pass: the operations made so far, the language the text is
/// in, and the last step that changed it.
struct Compiler<'a> {
    ops: Vec<Op>,
    lang: Option<String>,
    chinese: ChineseScript,
    changed: Option<(usize, Option<String>)>,
    ctx: &'a Context<'a>,
}

impl Compiler<'_> {
    fn step(&mut self, index: usize, step: &StepSpec) -> StepState {
        let Some(kind) = Kind::of(&step.id) else {
            myna_core::info_log!("process", "dropping unknown step {:?}", step.id);
            return StepState::Unknown;
        };
        if !step.on {
            return StepState::Off;
        }
        if let Some(reason) = misfit(kind, step, self.lang.as_deref()) {
            let move_above = self
                .changed
                .as_ref()
                .filter(|(_, before)| misfit(kind, step, before.as_deref()).is_none())
                .map(|(at, _)| *at);
            return StepState::NotApplicable {
                reason: reason.into(),
                move_above,
            };
        }
        match kind {
            Kind::Tidy => self.tidy(),
            Kind::Translate => self.translate(index, step),
            Kind::Script => self.script(step),
        }
    }

    fn tidy(&mut self) -> StepState {
        let language = if self.lang.is_some() {
            Lang::En
        } else {
            Lang::Any
        };
        self.ops.push(Op::Rule {
            rule: Rule::Disfluency(language),
        });
        self.ops.push(Op::Rule {
            rule: Rule::Vocabulary,
        });
        self.ask(Request::cleanup(None), Check::ConservativeEdit)
    }

    fn translate(&mut self, index: usize, step: &StepSpec) -> StepState {
        let to = step
            .text("to")
            .map(myna_core::chains::primary_language)
            .expect("misfit checked it");
        let from = self.lang.clone().unwrap_or_else(|| "en".into());
        let check = Check::Usable;
        let state = match pivot_legs(&from, &to, self.ctx) {
            Some((first, second)) => {
                let missing = [&first, &second]
                    .into_iter()
                    .find(|leg| (self.ctx.unavailable)(leg));
                let state = missing.map_or(StepState::Applicable, |leg| StepState::Unavailable {
                    request: leg.clone(),
                });
                self.ops.push(Op::Pivot {
                    first,
                    second,
                    check,
                });
                state
            }
            None => self.ask(Request::translate(&from, &to), check),
        };
        self.changed = Some((index, self.lang.replace(to)));
        state
    }

    fn script(&mut self, step: &StepSpec) -> StepState {
        self.chinese = step
            .text("mode")
            .and_then(ChineseScript::from_nick)
            .expect("misfit checked it");
        StepState::Applicable
    }

    /// Add a model step: unavailable when no installed model covers it.
    fn ask(&mut self, request: Request, check: Check) -> StepState {
        let state = if (self.ctx.unavailable)(&request) {
            StepState::Unavailable {
                request: request.clone(),
            }
        } else {
            StepState::Applicable
        };
        self.ops.push(Op::Ask { request, check });
        state
    }
}

/// The graph a preset names: its steps compiled for unknown language.
pub fn preset_graph(chain: ProcessingChain) -> Graph {
    compile(&preset_steps(chain), &Context::default()).graph
}

/// The two legs that translate `from` to `to` through English, when no
/// installed model translates them directly and at least one leg is
/// installed. A leg that is not installed is the step's missing request.
fn pivot_legs(from: &str, to: &str, ctx: &Context) -> Option<(Request, Request)> {
    let direct = Request::translate(from, to);
    if from == "en" || to == "en" || !(ctx.uncovered)(&direct) {
        return None;
    }
    let half = direct.run_budget / 2;
    let leg = |from, to| {
        let mut request = Request::translate(from, to);
        request.run_budget = half;
        request
    };
    let (first, second) = (leg(from, "en"), leg("en", to));
    (!((ctx.uncovered)(&first) && (ctx.uncovered)(&second))).then_some((first, second))
}

/// Route by what the field is: an address, a sentence in progress or text
/// that starts one.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::graph::Op;

    fn spec(ids: &[&str]) -> Vec<StepSpec> {
        ids.iter().map(|t| StepSpec::parse(t)).collect()
    }

    fn compiled(ids: &[&str], language: Option<&str>) -> Compiled {
        compile(
            &spec(ids),
            &Context {
                language,
                ..Context::default()
            },
        )
    }

    fn states(c: &Compiled) -> Vec<(&str, &'static str)> {
        c.report
            .iter()
            .map(|r| {
                let state = match r.state {
                    StepState::Applicable => "on",
                    StepState::NotApplicable { .. } => "n/a",
                    StepState::Unavailable { .. } => "missing",
                    StepState::Unknown => "unknown",
                    StepState::Off => "off",
                };
                (r.id.as_str(), state)
            })
            .collect()
    }

    fn not_applicable(c: &Compiled, index: usize) -> Option<usize> {
        match &c.report[index].state {
            StepState::NotApplicable { move_above, .. } => *move_above,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_chain_runs_in_the_order_given() {
        let tidy_first = compiled(&["tidy", "translate=fr"], Some("en"));
        assert_eq!(states(&tidy_first), [("tidy", "on"), ("translate", "on")]);

        let translate_first = compiled(&["translate=fr", "tidy"], Some("en"));
        assert_eq!(
            states(&translate_first),
            [("translate", "on"), ("tidy", "n/a")]
        );
        assert_eq!(
            not_applicable(&translate_first, 1),
            Some(0),
            "move above it"
        );
        assert_ne!(translate_first.graph, tidy_first.graph);
    }

    #[test]
    fn a_misfit_the_dictation_language_causes_has_no_step_to_move_above() {
        let c = compiled(&["tidy"], Some("fr"));
        assert_eq!(not_applicable(&c, 0), None);
        // Moving above a translation to German would not make it Chinese.
        let c = compiled(&["translate=de", "script=simplified"], Some("en"));
        assert_eq!(not_applicable(&c, 1), None);
        // Above a translation away from Chinese it would apply.
        let c = compiled(&["translate=en", "script=simplified"], Some("zh"));
        assert_eq!(not_applicable(&c, 1), Some(0));
    }

    #[test]
    fn tidy_names_its_language_to_the_pause_rule() {
        let first = |c: &Compiled| match &c.graph.ops()[0] {
            Op::Rule { rule, .. } => *rule,
            op => panic!("{op:?}"),
        };
        assert_eq!(
            first(&compiled(&["tidy"], None)),
            Rule::Disfluency(Lang::Any)
        );
        assert_eq!(
            first(&compiled(&["tidy"], Some("en"))),
            Rule::Disfluency(Lang::En)
        );
    }

    #[test]
    fn script_follows_the_language_the_text_is_in() {
        for (language, steps, state) in [
            (Some("zh"), &["script=simplified"][..], "on"),
            (Some("yue-HK"), &["script=traditional-hongkong"], "on"),
            (None, &["script=traditional"], "on"),
            (Some("en"), &["script=simplified"], "n/a"),
            (Some("fr"), &["translate=zh", "script=simplified"], "on"),
            (Some("zh"), &["translate=en", "script=simplified"], "n/a"),
            (Some("zh"), &["script=klingon"], "n/a"),
            (Some("zh"), &["script"], "n/a"),
        ] {
            let c = compiled(steps, language);
            assert_eq!(
                c.report.last().unwrap().state == StepState::Applicable,
                state == "on"
            );
        }
        let c = compiled(&["script=traditional-taiwan"], Some("zh"));
        assert_eq!(c.script, ChineseScript::TraditionalTaiwan);
        assert_eq!(c.graph, Graph::passthrough(), "a stage after the graph");
        assert_eq!(
            compiled(&["script=simplified"], Some("en")).script,
            ChineseScript::AsRecognised
        );
    }

    #[test]
    fn an_unknown_step_is_dropped_and_degrades_the_chain() {
        let c = compiled(&["shout", "tidy", "shout", "whisper"], None);
        assert_eq!(
            states(&c),
            [
                ("shout", "unknown"),
                ("tidy", "on"),
                ("shout", "unknown"),
                ("whisper", "unknown")
            ]
        );
        assert_eq!(c.graph, preset_graph(ProcessingChain::TIDY));
    }
}
