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
use super::graph::{Builder, Graph, Needs, Src};
use super::pipeline::{Check, Request};
use super::rules::Rule;
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

    fn text(&self, key: &str) -> Option<&str> {
        self.params
            .get(key)
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// The steps this build knows, in their default insert order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Drop hesitations, false starts and repeats.
    Tidy,
    /// Translate to `to`.
    Translate,
    /// Write Chinese in `script`.
    Script,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Tidy, Kind::Translate, Kind::Script];

    fn of(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    fn id(self) -> &'static str {
        match self {
            Kind::Tidy => "tidy",
            Kind::Translate => "translate",
            Kind::Script => "script",
        }
    }

    fn param(self) -> Option<&'static str> {
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

/// The language code before any region: `fr` for `fr-CA`.
fn primary(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Why `kind` does not apply to text in `lang`, if it does not, in words
/// for the log and the bus; the judgement is `myna_core::chains::misfit`,
/// which Settings shares.
fn misfit(kind: Kind, step: &StepSpec, lang: Option<&str>) -> Option<&'static str> {
    use myna_core::chains::Misfit;
    let params: myna_core::chains::StepParams = step
        .params
        .iter()
        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
        .collect();
    Some(match myna_core::chains::misfit(kind.id(), &params, lang)? {
        Misfit::NotEnglish => "Applies when the text is English",
        Misfit::NoTarget => "Needs a language to translate to",
        Misfit::AlreadyInTarget => "The text is already in that language",
        Misfit::NotChinese => "Applies when the text is Chinese",
        Misfit::NoScript => "Needs a script to write",
    })
}

/// Compile `steps` in their order for dictation in `ctx.language`.
pub fn compile(steps: &[StepSpec], ctx: &Context) -> Compiled {
    let mut g = Builder::default();
    let mut at = Src::Input;
    let mut lang = ctx.language.map(primary);
    let mut chinese = ChineseScript::AsRecognised;
    // The last step that changed the language, and the language before it.
    let mut changed: Option<(usize, Option<String>)> = None;
    let mut report = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        let kind = Kind::of(&step.id);
        let state = match kind {
            _ if !step.on => StepState::Off,
            None => {
                myna_core::info_log!("process", "dropping unknown step {:?}", step.id);
                StepState::Unknown
            }
            Some(kind) => match misfit(kind, step, lang.as_deref()) {
                Some(reason) => StepState::NotApplicable {
                    reason: reason.into(),
                    move_above: changed
                        .as_ref()
                        .filter(|(_, before)| misfit(kind, step, before.as_deref()).is_none())
                        .map(|(at, _)| *at),
                },
                None => match kind {
                    Kind::Tidy => {
                        let language = if lang.is_some() { Lang::En } else { Lang::Any };
                        at = g.rule(at, Rule::Disfluency(language));
                        at = g.rule(at, Rule::Vocabulary);
                        let request = Request::cleanup(None);
                        let state = ask_state(&request, ctx);
                        at = g.ask(at, request, Check::ConservativeEdit, Needs::Sentence);
                        state
                    }
                    Kind::Translate => {
                        let to = step.text("to").map(primary).expect("misfit checked it");
                        let from = lang.clone().unwrap_or_else(|| "en".into());
                        let check = Check::Usable { translation: true };
                        let state = match pivot_legs(&from, &to, ctx) {
                            Some((first, second)) => {
                                let state = [&first, &second]
                                    .into_iter()
                                    .find(|leg| (ctx.unavailable)(leg))
                                    .map_or(StepState::Applicable, |leg| StepState::Unavailable {
                                        request: leg.clone(),
                                    });
                                at = g.pivot(at, first, second, check);
                                state
                            }
                            None => {
                                let direct = Request::translate(&from, &to);
                                let state = ask_state(&direct, ctx);
                                at = g.ask(at, direct, check, Needs::Sentence);
                                state
                            }
                        };
                        changed = Some((index, lang.replace(to)));
                        state
                    }
                    Kind::Script => {
                        chinese = step
                            .text("mode")
                            .and_then(ChineseScript::from_nick)
                            .expect("misfit checked it");
                        StepState::Applicable
                    }
                },
            },
        };
        report.push(StepReport {
            id: step.id.clone(),
            state,
        });
    }
    Compiled {
        graph: g.finish(at),
        script: chinese,
        report,
    }
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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::graph::{Id, Op};

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
            StepSpec::new("script").with("mode", "traditional")
        );
        assert_eq!(StepSpec::parse("tidy=1"), StepSpec::new("tidy"));
        assert_eq!(StepSpec::parse("translate="), StepSpec::new("translate"));
        assert_eq!(StepSpec::parse("shout=1"), StepSpec::new("shout"));
    }

    fn not_applicable(c: &Compiled, index: usize) -> Option<usize> {
        match &c.report[index].state {
            StepState::NotApplicable { move_above, .. } => *move_above,
            other => panic!("step {index} is {other:?}"),
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
        assert!(!translate_first.degraded());
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
    fn a_step_listed_twice_runs_twice_where_it_fits() {
        let c = compiled(&["tidy", "translate=fr", "tidy"], Some("en"));
        assert_eq!(
            states(&c),
            [("tidy", "on"), ("translate", "on"), ("tidy", "n/a")]
        );
        assert_eq!(not_applicable(&c, 2), Some(1));
        let c = compiled(&["translate=fr", "translate=de"], Some("en"));
        assert_eq!(states(&c), [("translate", "on"), ("translate", "on")]);
        let asks: Vec<_> = c
            .graph
            .requests()
            .into_iter()
            .map(|r| (r.params["language"].clone(), r.params["to"].clone()))
            .collect();
        assert_eq!(
            asks,
            [("en".into(), "fr".into()), ("fr".into(), "de".into())]
        );
    }

    #[test]
    fn an_empty_chain_and_an_off_step_change_nothing() {
        let empty = compiled(&[], Some("en"));
        assert_eq!(empty.graph, Graph::passthrough());
        assert!(empty.report.is_empty());
        let mut off = StepSpec::parse("tidy");
        off.on = false;
        let c = compile(&[off, StepSpec::parse("translate=fr")], &Context::default());
        assert_eq!(states(&c), [("tidy", "off"), ("translate", "on")]);
        assert_eq!(c.graph, compiled(&["translate=fr"], None).graph);
        assert!(!c.degraded(), "off is no fault");
    }

    #[test]
    fn a_chain_step_keeps_its_parameters_and_switch() {
        let step = myna_core::chains::ChainStep {
            on: false,
            ..myna_core::chains::ChainStep::new("translate").with("to", "de")
        };
        let spec = StepSpec::of_chain(&step);
        assert_eq!(spec.id, "translate");
        assert_eq!(spec.text("to"), Some("de"));
        assert!(!spec.on);
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
        let StepState::NotApplicable { reason, .. } =
            &compiled(&["tidy"], Some("fr")).report[0].state
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
                reason: "Needs a language to translate to".into(),
                move_above: None,
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
                ..Context::default()
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

    /// Compile `translate=to` for `language` with only `installed` pairs
    /// covered; `bound` are the ones a model also answers for.
    fn through(language: &str, to: &str, installed: &[(&str, &str)]) -> Compiled {
        let covers = |r: &Request| {
            let get = |k: &str| r.params.get(k).and_then(Value::as_str).unwrap_or("");
            installed.contains(&(get("language"), get("to")))
        };
        let uncovered = |r: &Request| !covers(r);
        compile(
            &spec(&[&format!("translate={to}")]),
            &Context {
                language: Some(language),
                unavailable: &uncovered,
                uncovered: &uncovered,
            },
        )
    }

    #[test]
    fn a_direct_model_is_used_before_a_pivot() {
        let c = through("fr", "de", &[("fr", "de"), ("fr", "en"), ("en", "de")]);
        assert_eq!(states(&c), [("translate", "on")]);
        assert_eq!(asks(&c), [("fr".into(), "de".into())]);
        assert!(matches!(c.graph.node(Id::first()), Op::Ask { .. }));
    }

    #[test]
    fn two_installed_legs_make_a_pivot_whose_budgets_add_up() {
        let c = through("fr", "de", &[("fr", "en"), ("en", "de")]);
        assert_eq!(states(&c), [("translate", "on")]);
        assert!(!c.degraded());
        assert_eq!(
            asks(&c),
            [("fr".into(), "en".into()), ("en".into(), "de".into())]
        );
        let Op::Pivot { first, second, .. } = c.graph.node(Id::first()) else {
            panic!("not a pivot");
        };
        let direct = Request::translate("fr", "de").run_budget;
        assert_eq!(first.request.run_budget + second.request.run_budget, direct);
        assert_eq!(first.check, Check::Usable { translation: true });
        assert_eq!(c.graph.needs(), Some(Needs::Sentence));
    }

    #[test]
    fn a_pivot_with_one_leg_missing_names_that_leg() {
        for (installed, missing) in [
            (&[("fr", "en")][..], ("en", "de")),
            (&[("en", "de")][..], ("fr", "en")),
        ] {
            let c = through("fr", "de", installed);
            assert_eq!(states(&c), [("translate", "missing")]);
            let StepState::Unavailable { request } = &c.report[0].state else {
                panic!("not unavailable");
            };
            assert_eq!(
                (
                    request.params["language"].as_str().unwrap(),
                    request.params["to"].as_str().unwrap()
                ),
                missing
            );
            assert!(c.degraded());
            assert!(matches!(c.graph.node(Id::first()), Op::Pivot { .. }));
        }
    }

    #[test]
    fn with_no_leg_installed_the_direct_pair_is_what_is_missing() {
        let c = through("fr", "de", &[("fr", "fa")]);
        assert_eq!(states(&c), [("translate", "missing")]);
        assert_eq!(asks(&c), [("fr".into(), "de".into())]);
    }

    #[test]
    fn english_is_never_pivoted_through_itself() {
        for (language, to) in [("en", "de"), ("fr", "en")] {
            let c = through(language, to, &[("en", "en")]);
            assert_eq!(asks(&c), [(language.into(), to.into())]);
            assert!(matches!(c.graph.node(Id::first()), Op::Ask { .. }));
        }
    }

    #[test]
    fn a_pivot_leaves_the_text_in_the_target_language_for_what_follows() {
        let c = compile(
            &spec(&["translate=de", "script=simplified"]),
            &Context {
                language: Some("fr"),
                unavailable: &|_| false,
                uncovered: &|r| r.params["to"] == "de" && r.params["language"] == "fr",
            },
        );
        assert_eq!(states(&c), [("translate", "on"), ("script", "n/a")]);
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
        assert!(c.degraded());
        assert_eq!(c.graph, Graph::preset(ProcessingChain::TIDY));
    }

    #[test]
    fn tidy_spells_the_vocabulary_once() {
        let c = compiled(&["tidy"], None);
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

    #[test]
    fn stage_order_here_is_the_order_settings_writes() {
        let ids: Vec<&str> = Kind::ALL.iter().map(|k| k.id()).collect();
        assert_eq!(ids, myna_core::processing::STEP_ORDER);
    }

    #[test]
    fn stored_steps_take_the_parameters_that_carry_their_id() {
        let params: myna_core::processing::Params = [
            ("translate.to", "de"),
            ("script.mode", "simplified"),
            ("translatex.to", "no"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let ids = ["tidy", "translate", "script", "from-the-future"].map(String::from);
        assert_eq!(
            stored_steps(&ids, &params),
            [
                StepSpec::new("tidy"),
                StepSpec::new("translate").with("to", "de"),
                StepSpec::new("script").with("mode", "simplified"),
                StepSpec::new("from-the-future"),
            ]
        );
        let c = compile(&stored_steps(&ids, &params), &Context::default());
        assert!(c.degraded(), "the unknown id is reported");
    }

    #[test]
    fn a_history_label_names_a_preset_or_joins_the_steps() {
        for chain in ProcessingChain::ALL {
            assert_eq!(label(&preset_steps(chain)), chain.nick());
        }
        let german = vec![
            StepSpec::new("tidy"),
            StepSpec::new("translate").with("to", "de"),
        ];
        assert_eq!(label(&german), "tidy+translate");
        assert_eq!(label(&spec(&["tidy", "shout"])), "tidy+shout");
    }

    #[test]
    fn describe_lists_each_step_with_its_parameter() {
        assert_eq!(describe(&[]), "(none)");
        assert_eq!(
            describe(&spec(&["tidy", "translate=fr", "script=traditional"])),
            "tidy, translate=fr, script=traditional"
        );
    }
}
