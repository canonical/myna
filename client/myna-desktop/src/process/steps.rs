//! Processing as an ordered list of steps, compiled to a [`Graph`].
//!
//! The chain runs in the order the user gave it. `myna_core::graph::solve`
//! says how each step runs on the text as it stands (which models, which
//! script conversions in between), and this turns that into operations.
//! A step that no implementation can run on the text at that point is
//! skipped statically: not applicable is no fault, and when moving it above
//! the step that changed the language would fix it, the report says which.

use myna_core::graph::{self, Implementation, Outcome, Script, Step, Tongue};
use myna_core::{ChineseScript, ProcessingChain};
use serde_json::Value;

use super::disfluency::Lang;
use super::graph::{Graph, Op};
use super::pipeline::{Check, Request};
use super::resolve::Candidate;
use super::rules::Rule;

use super::spec::Kind;
pub use super::spec::{StepReport, StepSpec, StepState};

/// What the compiler needs to know besides the steps.
pub struct Context<'a> {
    /// The speech model's language; `None` when not known.
    pub language: Option<&'a str>,
    /// Every model of every connected share: what the chain can use.
    pub models: &'a [Candidate],
    /// Whether no model answers a request, as far as is known.
    pub unavailable: &'a dyn Fn(&Request) -> bool,
}

impl Default for Context<'_> {
    fn default() -> Self {
        Self {
            language: None,
            models: &[],
            unavailable: &|_| false,
        }
    }
}

/// A compiled step list.
#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub graph: Graph,
    /// One entry per step, at the step's index in the chain.
    pub report: Vec<StepReport>,
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

/// The scripts Chinese is written in, as OpenCC converts to them.
const FORMS: [(ChineseScript, &str); 4] = [
    (ChineseScript::Simplified, "Hans"),
    (ChineseScript::Traditional, "Hant"),
    (ChineseScript::TraditionalTaiwan, "Hant-TW"),
    (ChineseScript::TraditionalHongKong, "Hant-HK"),
];

fn form(script: ChineseScript) -> Option<Script> {
    FORMS
        .iter()
        .find(|(s, _)| *s == script)
        .map(|(_, tag)| Script::Known((*tag).into()))
}

fn form_of(tongue: &Tongue) -> Option<ChineseScript> {
    FORMS
        .iter()
        .find(|(_, tag)| tongue.script == Script::Known((*tag).into()))
        .map(|(s, _)| *s)
}

/// The type of the dictation: an unknown language reads as English, and
/// Chinese as mixed scripts, for no speech model says which it wrote.
fn dictated(language: Option<&str>) -> Tongue {
    let script = match language {
        Some(_) if myna_core::chains::may_be_chinese(language) => Script::Mixed,
        _ => Script::Unspecified,
    };
    let primary = language.map_or_else(|| "en".into(), myna_core::chains::primary_language);
    Tongue::new(&primary, script)
}

/// What the connected models and the built-in converters can do.
fn offered(ctx: &Context) -> Vec<Implementation> {
    let covers =
        |c: &Candidate, key: &str| c.capability.covers.get(key).cloned().unwrap_or_default();
    let imp = |kind, accepts: Tongue, produces: Tongue, latency_ms, quality| Implementation {
        id: format!("{accepts}-{produces}"),
        kind,
        accepts,
        produces,
        installed: true,
        latency_ms,
        quality,
    };
    // An unknown language, read as English, may still be Chinese.
    let mut out = graph::opencc(
        ["zh", "yue", "cmn"]
            .into_iter()
            .chain(ctx.language.is_none().then_some("en")),
    );
    let models = |task: &'static str| ctx.models.iter().filter(move |c| c.capability.task == task);
    let english = Tongue::new("en", Script::Unspecified);
    out.push(imp(graph::Kind::Tidy, english.clone(), english, 0, 0));
    for c in models("text.translate") {
        let ms = u32::try_from(c.capability.latency_ms.unwrap_or(0)).unwrap_or(u32::MAX);
        let quality = (c.capability.quality.unwrap_or(0.0) * 1000.0) as u32;
        for (from, to) in covers(c, "language")
            .iter()
            .flat_map(|f| covers(c, "to").into_iter().map(move |t| (f.clone(), t)))
            .filter(|(f, t)| f != t)
        {
            let (from, to) = (Tongue::parse(&from), Tongue::parse(&to));
            out.push(imp(graph::Kind::Translate, from, to, ms, quality));
        }
    }
    out
}

/// The step to solve, or the state of one that needs no solving.
fn plan(step: &StepSpec) -> Result<Step, StepState> {
    let not_applicable = |reason: &str| {
        Err(StepState::NotApplicable {
            reason: reason.into(),
            move_above: None,
        })
    };
    let Some(kind) = Kind::of(&step.id) else {
        myna_core::info_log!("process", "dropping unknown step {:?}", step.id);
        return Err(StepState::Unknown);
    };
    if !step.on {
        return Err(StepState::Off);
    }
    match kind {
        Kind::Tidy => Ok(Step::Tidy),
        Kind::Translate => match step.text("to") {
            Some(to) => Ok(Step::Translate {
                to: Tongue::parse(to),
            }),
            None => not_applicable("Needs a language to translate to"),
        },
        Kind::Script => match step.text("mode").and_then(ChineseScript::from_nick) {
            Some(ChineseScript::AsRecognised) => Err(StepState::Applicable),
            Some(script) => Ok(Step::Script {
                to: form(script).expect("every script but as-recognised has a form"),
            }),
            None => not_applicable("Needs a script to write"),
        },
    }
}

/// Compile `steps` in their order for dictation in `ctx.language`.
pub fn compile(steps: &[StepSpec], ctx: &Context) -> Compiled {
    let impls = offered(ctx);
    let mut report = Vec::new();
    let mut solved = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let state = plan(step).map_or_else(
            |state| state,
            |s| {
                solved.push((index, s));
                StepState::Applicable
            },
        );
        report.push(StepReport {
            id: step.id.clone(),
            state,
        });
    }
    let chain: Vec<Step> = solved.iter().map(|(_, s)| s.clone()).collect();
    let mut at = dictated(ctx.language);
    let mut changed: Option<(usize, Tongue)> = None;
    let mut ops = Vec::new();
    for ((index, step), outcome) in solved.iter().zip(graph::solve(&chain, &at, &impls).steps) {
        let translating = matches!(step, Step::Translate { .. });
        let state = match outcome {
            Outcome::Runs(path) | Outcome::Needs(path) => {
                let before = std::mem::replace(&mut at, path.produces.clone());
                if translating {
                    changed = Some((*index, before));
                }
                let (mut made, missing) = path_ops(&path, &impls, ctx, translating);
                ops.append(&mut made);
                missing.map_or(StepState::Applicable, |request| StepState::Unavailable {
                    request,
                })
            }
            Outcome::Skipped(reason) => match step {
                // No model for the pair yet: the step stays, fails open, and
                // says what to install.
                Step::Translate { to } if !to.accepts(&at) => {
                    let request = Request::translate(&at.to_string(), &to.to_string());
                    ops.push(Op::Ask {
                        request: request.clone(),
                        check: Check::Usable,
                    });
                    changed = Some((*index, std::mem::replace(&mut at, to.clone())));
                    StepState::Unavailable { request }
                }
                _ => StepState::NotApplicable {
                    reason,
                    move_above: changed
                        .as_ref()
                        .filter(|(_, before)| {
                            let alone = graph::solve(std::slice::from_ref(step), before, &impls);
                            !matches!(alone.steps[0], Outcome::Skipped(_))
                        })
                        .map(|(at, _)| *at),
                },
            },
        };
        report[*index].state = state;
    }
    Compiled {
        graph: Graph::of(ops),
        report,
    }
}

/// The operations of one step's path, and the first model request nothing
/// answers. A translation through several models is all or nothing.
fn path_ops(
    path: &graph::Path,
    impls: &[Implementation],
    ctx: &Context,
    translating: bool,
) -> (Vec<Op>, Option<Request>) {
    let mut ops = Vec::new();
    for hop in &path.hops {
        let imp = &impls[hop.implementation];
        let ask = |request, check| Op::Ask { request, check };
        match imp.kind {
            graph::Kind::Transcode => ops.extend(form_of(&imp.produces).map(Op::Convert)),
            graph::Kind::Translate => {
                let request =
                    Request::translate(&imp.accepts.to_string(), &imp.produces.to_string());
                ops.push(ask(request, Check::Usable));
            }
            graph::Kind::Tidy => {
                let language = if ctx.language.is_some() {
                    Lang::En
                } else {
                    Lang::Any
                };
                ops.push(Op::Rule {
                    rule: Rule::Disfluency(language),
                });
                ops.push(Op::Rule {
                    rule: Rule::Vocabulary,
                });
                ops.push(ask(Request::cleanup(None), Check::ConservativeEdit));
            }
        }
    }
    let requests = |ops: &[Op]| {
        ops.iter()
            .filter_map(|op| match op {
                Op::Ask { request, .. } => Some(request.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let asking = requests(&ops);
    let missing = asking.iter().find(|r| (ctx.unavailable)(r)).cloned();
    for op in &mut ops {
        if let Op::Ask { request, .. } = op {
            request.run_budget /= u32::try_from(asking.len()).unwrap_or(1);
        }
    }
    if translating && ops.len() > 1 {
        ops = vec![Op::Chain(ops)];
    }
    (ops, missing)
}

/// The graph a preset names: its steps compiled for unknown language.
pub fn preset_graph(chain: ProcessingChain) -> Graph {
    compile(&preset_steps(chain), &Context::default()).graph
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ids: &[&str]) -> Vec<StepSpec> {
        ids.iter().map(|t| StepSpec::parse(t)).collect()
    }

    fn pair(from: &str, to: &str) -> Candidate {
        Candidate {
            socket: "/s".into(),
            share: "myna-translation".into(),
            capability: serde_json::from_value(serde_json::json!({
                "id": format!("{from}-{to}"), "task": "text.translate",
                "covers": {"language": [from], "to": [to]}, "latency_ms": 20,
            }))
            .unwrap(),
        }
    }

    fn compiled(ids: &[&str], language: Option<&str>, pairs: &[(&str, &str)]) -> Compiled {
        let models: Vec<Candidate> = pairs.iter().map(|(f, t)| pair(f, t)).collect();
        compile(
            &spec(ids),
            &Context {
                language,
                models: &models,
                ..Context::default()
            },
        )
    }

    fn asks(c: &Compiled) -> Vec<(String, String)> {
        c.graph
            .requests()
            .iter()
            .map(|r| {
                let param = |k: &str| r.params[k].as_str().unwrap().to_owned();
                (param("language"), param("to"))
            })
            .collect()
    }

    fn states(c: &Compiled) -> Vec<&'static str> {
        c.report
            .iter()
            .map(|r| match r.state {
                StepState::Applicable => "on",
                StepState::NotApplicable { .. } => "n/a",
                StepState::Unavailable { .. } => "missing",
                StepState::Unknown => "unknown",
                StepState::Off => "off",
            })
            .collect()
    }

    fn move_above(c: &Compiled, index: usize) -> Option<usize> {
        match &c.report[index].state {
            StepState::NotApplicable { move_above, .. } => *move_above,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_chain_runs_in_the_order_given() {
        let tidy_first = compiled(&["tidy", "translate=fr"], Some("en"), &[("en", "fr")]);
        assert_eq!(states(&tidy_first), ["on", "on"]);

        let translate_first = compiled(&["translate=fr", "tidy"], Some("en"), &[("en", "fr")]);
        assert_eq!(states(&translate_first), ["on", "n/a"]);
        assert_eq!(move_above(&translate_first, 1), Some(0));
        assert_ne!(translate_first.graph, tidy_first.graph);
        assert_eq!(move_above(&compiled(&["tidy"], Some("fr"), &[]), 0), None);
    }

    #[test]
    fn a_translation_with_no_direct_model_goes_through_english() {
        let c = compiled(&["translate=fr"], Some("fa"), &[("fa", "en"), ("en", "fr")]);
        assert_eq!(
            asks(&c),
            [("fa".into(), "en".into()), ("en".into(), "fr".into())]
        );
        assert_eq!(states(&c), ["on"]);
        let direct = compiled(
            &["translate=fr"],
            Some("fa"),
            &[("fa", "en"), ("en", "fr"), ("fa", "fr")],
        );
        assert_eq!(asks(&direct), [("fa".into(), "fr".into())]);
    }

    #[test]
    fn chinese_into_english_is_written_traditional_first() {
        let c = compiled(&["translate=en"], Some("zh"), &[("zh-Hant", "en")]);
        assert_eq!(asks(&c), [("zh-Hant".into(), "en".into())]);
        assert!(c.graph.converts());
        let Op::Chain(ops) = &c.graph.ops()[0] else {
            panic!("{c:?}")
        };
        assert_eq!(ops[0], Op::Convert(ChineseScript::Traditional));
    }

    #[test]
    fn a_script_step_converts_only_chinese() {
        for (language, steps, converts) in [
            (Some("zh"), &["script=simplified"][..], true),
            (Some("yue-HK"), &["script=traditional-hongkong"], true),
            (None, &["script=traditional"], true),
            (Some("en"), &["script=simplified"], false),
            (Some("zh"), &["script=klingon"], false),
        ] {
            assert_eq!(
                compiled(steps, language, &[]).graph.converts(),
                converts,
                "{language:?} {steps:?}"
            );
        }
    }

    #[test]
    fn an_unknown_step_is_dropped_and_degrades_the_chain() {
        let c = compiled(&["shout", "tidy", "shout", "whisper"], None, &[]);
        assert_eq!(states(&c), ["unknown", "on", "unknown", "unknown"]);
        assert_eq!(c.graph, preset_graph(ProcessingChain::TIDY));
    }
}
