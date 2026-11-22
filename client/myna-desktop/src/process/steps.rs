//! Processing as an ordered list of steps, compiled to a [`Graph`].
//!
//! The user switches steps on; where each runs is fixed by its stage, so no
//! list can order them badly. A step declares the language it reads and the
//! language it leaves, and one whose input does not match the text at that
//! point is skipped statically: not applicable is no fault.

use myna_core::{ChineseScript, ProcessingChain};
use myna_platform::text_input::FieldPurpose;
use serde_json::Value;

use super::disfluency::Lang;
use super::graph::{Builder, Graph, Needs, Src, When};
use super::pipeline::{Check, Request};
use super::rules::Rule;
use super::script;
use super::wire::Params;

/// A step as stored: a stable id and its parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepSpec {
    pub id: String,
    pub params: Params,
}

impl StepSpec {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            params: Params::new(),
        }
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.params.insert(key.into(), value.into());
        self
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

    fn text(&self, key: &str) -> Option<&str> {
        self.params
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// The steps this build knows, in stage order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Stage 1: fit the text to the field it lands in.
    Fit,
    /// Stage 2: drop hesitations, false starts and repeats.
    Tidy,
    /// Stage 3: translate to `to`.
    Translate,
    /// Stage 4: write Chinese in `script`.
    Script,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Fit, Kind::Tidy, Kind::Translate, Kind::Script];

    fn of(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Fit => "fit",
            Kind::Tidy => "tidy",
            Kind::Translate => "translate",
            Kind::Script => "script",
        }
    }

    fn param(self) -> Option<&'static str> {
        match self {
            Kind::Translate => Some("to"),
            Kind::Script => Some("script"),
            Kind::Fit | Kind::Tidy => None,
        }
    }
}

/// What the compiler became of one step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepState {
    /// In the graph.
    Applicable,
    /// Left out because the text it would read is not what it reads; not a
    /// fault.
    NotApplicable { reason: String },
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

/// What the compiler needs to know besides the steps.
pub struct Context<'a> {
    /// The speech model's language; `None` when not known.
    pub language: Option<&'a str>,
    /// Whether no model answers a request, as far as is known.
    pub unavailable: &'a dyn Fn(&Request) -> bool,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            language: None,
            unavailable: &|_| false,
        }
    }
}

/// A compiled step list.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub graph: Graph,
    /// How Chinese is written after the graph.
    pub script: ChineseScript,
    /// One entry per step in stage order, then the unknown ones.
    pub report: Vec<StepReport>,
}

impl Compiled {
    /// Whether a step is unknown or has no model: what the chain status
    /// has to name. Not applicable is neither.
    pub fn degraded(&self) -> bool {
        self.report
            .iter()
            .any(|r| matches!(r.state, StepState::Unavailable { .. } | StepState::Unknown))
    }
}

/// The steps a preset is.
pub fn preset_steps(chain: ProcessingChain) -> Vec<StepSpec> {
    match chain {
        ProcessingChain::TIDY => vec![StepSpec::new("tidy")],
        ProcessingChain::TIDY_FRENCH => {
            vec![
                StepSpec::new("tidy"),
                StepSpec::new("translate").with("to", "fr"),
            ]
        }
        ProcessingChain::KEYWORD => vec![StepSpec::new("fit")],
        _ => Vec::new(),
    }
}

/// The language code before any region: `fr` for `fr-CA`.
fn primary(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Compile `steps` in stage order for dictation in `ctx.language`.
pub fn compile(steps: &[StepSpec], ctx: &Context) -> Compiled {
    let mut known: Vec<(Kind, &StepSpec)> = Vec::new();
    let mut report = Vec::new();
    let mut unknown = Vec::new();
    for step in steps {
        match Kind::of(&step.id) {
            Some(kind) if known.iter().all(|(k, _)| *k != kind) => known.push((kind, step)),
            Some(_) => {}
            None if unknown.contains(&step.id) => {}
            None => {
                myna_core::info_log!("process", "dropping unknown step {:?}", step.id);
                unknown.push(step.id.clone());
            }
        }
    }
    known.sort_by_key(|(kind, _)| *kind as u8);

    let mut g = Builder::default();
    let mut at = Src::Input;
    let mut lang = ctx.language.map(primary);
    let mut spelled = false;
    let mut chinese = ChineseScript::AsRecognised;
    for (kind, step) in known {
        let state = match kind {
            Kind::Fit => {
                at = fit(&mut g, at);
                spelled = true;
                StepState::Applicable
            }
            Kind::Tidy if lang.as_deref().is_some_and(|l| l != "en") => StepState::NotApplicable {
                reason: "Applies when the text is English".into(),
            },
            Kind::Tidy => {
                let language = if lang.is_some() { Lang::En } else { Lang::Any };
                at = g.rule(at, Rule::Disfluency(language));
                if !spelled {
                    at = g.rule(at, Rule::Vocabulary);
                }
                let request = Request::cleanup(None);
                let state = ask_state(&request, ctx);
                at = g.ask(at, request, Check::ConservativeEdit, Needs::Sentence);
                state
            }
            Kind::Translate => match step.text("to").map(primary) {
                None => StepState::NotApplicable {
                    reason: "Needs a language to translate to".into(),
                },
                Some(to) if lang.as_deref() == Some(to.as_str()) => StepState::NotApplicable {
                    reason: "The text is already in that language".into(),
                },
                Some(to) => {
                    let request = Request::translate(lang.as_deref().unwrap_or("en"), &to);
                    let state = ask_state(&request, ctx);
                    at = g.ask(
                        at,
                        request,
                        Check::Usable { translation: true },
                        Needs::Sentence,
                    );
                    lang = Some(to);
                    state
                }
            },
            Kind::Script => match step.text("script").and_then(ChineseScript::from_nick) {
                _ if !script::chinese(lang.as_deref()) => StepState::NotApplicable {
                    reason: "Applies when the text is Chinese".into(),
                },
                None => StepState::NotApplicable {
                    reason: "Needs a script to write".into(),
                },
                Some(wanted) => {
                    chinese = wanted;
                    StepState::Applicable
                }
            },
        };
        report.push(StepReport {
            id: kind.id().into(),
            state,
        });
    }
    report.extend(unknown.into_iter().map(|id| StepReport {
        id,
        state: StepState::Unknown,
    }));
    Compiled {
        graph: g.finish(at),
        script: chinese,
        report,
    }
}

fn ask_state(request: &Request, ctx: &Context) -> StepState {
    if (ctx.unavailable)(request) {
        StepState::Unavailable {
            request: request.clone(),
        }
    } else {
        StepState::Applicable
    }
}

/// Route by what the field is: an address, a sentence in progress or text
/// that starts one.
fn fit(g: &mut Builder, from: Src) -> Src {
    let address = g.rule(from, Rule::Address);
    let go_on = g.rule(from, Rule::Continue);
    // After the recasing, so a term keeps its capitals; an address is no
    // prose and keeps none of them.
    let go_on = g.rule(go_on, Rule::Vocabulary);
    let heard = g.rule(from, Rule::Vocabulary);
    g.route(
        vec![
            (
                When::Purpose(&[FieldPurpose::Url, FieldPurpose::Email]),
                address,
            ),
            (When::MidSentence, go_on),
        ],
        heard,
    )
}

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
                };
                (r.id.as_str(), state)
            })
            .collect()
    }

    fn asks(c: &Compiled) -> Vec<(String, String)> {
        c.graph
            .requests()
            .iter()
            .map(|r| {
                let get = |k: &str| r.params.get(k).and_then(Value::as_str).unwrap_or("");
                (get("language").to_owned(), get("to").to_owned())
            })
            .collect()
    }

    #[test]
    fn presets_are_their_step_lists() {
        for (chain, steps) in [
            (ProcessingChain::EXACT, &[][..]),
            (ProcessingChain::TIDY, &["tidy"][..]),
            (ProcessingChain::TIDY_FRENCH, &["tidy", "translate=fr"][..]),
            (ProcessingChain::KEYWORD, &["fit"][..]),
        ] {
            assert_eq!(preset_steps(chain), spec(steps), "{}", chain.nick());
            assert_eq!(compiled(steps, None).graph, Graph::preset(chain));
        }
        assert_eq!(
            compiled(&[], None).graph,
            Graph::passthrough(),
            "no steps type as heard"
        );
    }

    #[test]
    fn a_step_takes_its_parameter_from_id_equals_value() {
        assert_eq!(
            StepSpec::parse(" translate = de "),
            StepSpec::new("translate").with("to", "de")
        );
        assert_eq!(
            StepSpec::parse("script=traditional"),
            StepSpec::new("script").with("script", "traditional")
        );
        assert_eq!(StepSpec::parse("tidy=1"), StepSpec::new("tidy"));
        assert_eq!(StepSpec::parse("translate="), StepSpec::new("translate"));
        assert_eq!(StepSpec::parse("shout=1"), StepSpec::new("shout"));
    }

    #[test]
    fn stage_order_does_not_depend_on_input_order() {
        let all = ["fit", "tidy", "translate=fr", "script=simplified"];
        let want = compiled(&all, Some("en"));
        assert_eq!(
            states(&want),
            [
                ("fit", "on"),
                ("tidy", "on"),
                ("translate", "on"),
                ("script", "n/a")
            ]
        );
        // Every permutation of four steps.
        let mut order = [0, 1, 2, 3];
        let mut seen = 0;
        loop {
            let ids: Vec<_> = order.iter().map(|&i| all[i]).collect();
            assert_eq!(compiled(&ids, Some("en")), want, "{ids:?}");
            seen += 1;
            let Some(i) = (0..3).rev().find(|&i| order[i] < order[i + 1]) else {
                break;
            };
            let j = (i + 1..4).rev().find(|&j| order[j] > order[i]).unwrap();
            order.swap(i, j);
            order[i + 1..].reverse();
        }
        assert_eq!(seen, 24);
    }

    #[test]
    fn a_step_listed_twice_runs_once() {
        assert_eq!(
            compiled(&["tidy", "translate=fr", "tidy"], None),
            compiled(&["tidy", "translate=fr"], None)
        );
    }

    #[test]
    fn tidy_reads_english_only() {
        for (language, state) in [
            (None, "on"),
            (Some("en"), "on"),
            (Some("en-GB"), "on"),
            (Some("EN_us"), "on"),
            (Some("fr"), "n/a"),
            (Some("zh"), "n/a"),
        ] {
            let c = compiled(&["tidy"], language);
            assert_eq!(states(&c), [("tidy", state)], "{language:?}");
            assert_eq!(c.graph == Graph::passthrough(), state == "n/a");
            assert!(!c.degraded(), "not applicable is no fault");
        }
        let StepState::NotApplicable { reason } = &compiled(&["tidy"], Some("fr")).report[0].state
        else {
            panic!()
        };
        assert!(reason.contains("English"));
    }

    #[test]
    fn tidy_names_its_language_to_the_pause_rule() {
        let first = |c: &Compiled| match c.graph.node(crate::process::graph::Id::first()) {
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
    fn translate_goes_from_the_dictation_language_to_its_target() {
        for (language, to, from, target) in [
            (Some("en"), "fr", "en", "fr"),
            (Some("de-AT"), "FR-ca", "de", "fr"),
            (None, "de", "en", "de"),
            (Some("zh"), "en", "zh", "en"),
        ] {
            let c = compiled(&[&format!("translate={to}")], language);
            assert_eq!(states(&c), [("translate", "on")]);
            assert_eq!(asks(&c), [(from.into(), target.into())]);
        }
    }

    #[test]
    fn translate_to_the_language_spoken_or_to_nothing_is_skipped() {
        for (steps, language) in [
            (vec!["translate=fr"], Some("fr")),
            (vec!["translate=FR"], Some("fr-CA")),
            (vec!["translate"], Some("en")),
            (vec!["translate="], None),
        ] {
            let c = compiled(&steps, language);
            assert_eq!(states(&c), [("translate", "n/a")], "{steps:?}");
            assert_eq!(c.graph, Graph::passthrough());
        }
        let mut blank = StepSpec::new("translate");
        blank.params.insert("to".into(), Value::Null);
        assert_eq!(
            compile(&[blank], &Context::default()).report[0].state,
            StepState::NotApplicable {
                reason: "Needs a language to translate to".into()
            }
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
            assert!(!c.degraded());
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
    fn a_step_without_a_model_is_in_the_graph_and_named() {
        let missing = |r: &Request| r.task == "text.translate";
        let c = compile(
            &spec(&["tidy", "translate=fr"]),
            &Context {
                language: None,
                unavailable: &missing,
            },
        );
        assert_eq!(states(&c), [("tidy", "on"), ("translate", "missing")]);
        assert_eq!(
            c.report[1].state,
            StepState::Unavailable {
                request: Request::translate("en", "fr")
            }
        );
        assert!(c.degraded());
        assert_eq!(c.graph, Graph::preset(ProcessingChain::TIDY_FRENCH));

        let all = |_: &Request| true;
        let c = compile(
            &spec(&["tidy"]),
            &Context {
                unavailable: &all,
                ..Context::default()
            },
        );
        assert_eq!(states(&c), [("tidy", "missing")]);
    }

    #[test]
    fn an_unknown_step_is_dropped_and_degrades_the_chain() {
        let c = compiled(&["shout", "tidy", "shout", "whisper"], None);
        assert_eq!(
            states(&c),
            [("tidy", "on"), ("shout", "unknown"), ("whisper", "unknown")]
        );
        assert!(c.degraded());
        assert_eq!(c.graph, Graph::preset(ProcessingChain::TIDY));
    }

    #[test]
    fn fit_then_tidy_spells_the_vocabulary_once() {
        let c = compiled(&["fit", "tidy"], None);
        let vocab = c
            .graph
            .plan(&crate::process::Field::default())
            .into_iter()
            .filter(|&id| {
                matches!(
                    c.graph.node(id),
                    Op::Rule {
                        rule: Rule::Vocabulary,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(vocab, 1, "the plain arm and tidy share one spelling");
        assert_eq!(c.graph.needs(), Some(Needs::Sentence));
    }

    #[test]
    fn language_tables() {
        assert_eq!(primary("fr-CA"), "fr");
        assert_eq!(primary("ZH_tw"), "zh");
        assert_eq!(primary(""), "");
    }
}
