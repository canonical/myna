//! Graph specs for `--graph` and `--sweep`: a preset id, or a comma list of
//! steps built on the daemon's own [`Graph`] builder. A dev-tool concern:
//! Settings only ever stores a preset id.

use myna_core::{ChineseScript, ProcessingChain};
use myna_desktop::process::disfluency::Lang;
use myna_desktop::process::graph::{Builder, Graph, Src};
use myna_desktop::process::pipeline::{Check, Request};
use myna_desktop::process::rules::Rule;
use myna_desktop::process::steps::{compile, Context, StepSpec, StepState};

/// What a spec names: the graph, and how Chinese is written after it.
#[derive(Clone, Debug)]
pub struct Spec {
    pub label: String,
    pub graph: Graph,
    pub script: ChineseScript,
}

/// `--steps`: a comma list of step ids (`tidy, translate=fr,
/// script=simplified`) compiled in the order given for dictation in
/// `language`, by the same compiler the daemon runs.
pub fn parse_steps(list: &str, language: Option<&str>) -> Result<Spec, String> {
    let steps: Vec<_> = list
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(StepSpec::parse)
        .collect();
    let compiled = compile(
        &steps,
        &Context {
            language,
            ..Context::default()
        },
    );
    if let Some(bad) = compiled
        .report
        .iter()
        .find(|r| r.state == StepState::Unknown)
    {
        return Err(format!(
            "unknown step {:?} (want tidy, translate=LANG, script=NICK)",
            bad.id
        ));
    }
    Ok(Spec {
        label: format!("steps: {}", list.trim()),
        graph: compiled.graph,
        script: compiled.script,
    })
}

/// The steps a list may name, for the usage text and error messages.
pub const STEPS: &str =
    "disfluency, vocabulary, cleanup, translate=FR | translate=EN:FR, address, continue, script=NICK";

/// The most steps a sweep permutes: 5 steps are 325 graphs, 6 would be 1956.
const SWEEP_MAX: usize = 5;

pub fn parse(spec: &str) -> Result<Spec, String> {
    let label = spec.trim();
    let mut script = ChineseScript::AsRecognised;
    let mut steps = Vec::new();
    for token in label.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match token.split_once('=') {
            Some(("script", nick)) => {
                script = ChineseScript::from_nick(nick).ok_or_else(|| {
                    let known: Vec<_> = ChineseScript::ALL.iter().map(|s| s.nick()).collect();
                    format!("unknown script {nick:?} (want {})", known.join("|"))
                })?;
            }
            _ => steps.push(token),
        }
    }
    let graph = match steps.as_slice() {
        [] => Graph::preset(ProcessingChain::EXACT),
        [one] if ProcessingChain::from_nick(one).is_some() => {
            Graph::preset(ProcessingChain::from_nick(one).expect("checked"))
        }
        many => chain(many)?,
    };
    Ok(Spec {
        label: if label.is_empty() { "exact" } else { label }.to_owned(),
        graph,
        script,
    })
}

/// Steps run one after another, each reading the one before.
fn chain(steps: &[&str]) -> Result<Graph, String> {
    let mut g = Builder::default();
    let mut at = Src::Input;
    for step in steps {
        at = match step.split_once('=').unwrap_or((step, "")) {
            ("disfluency", "") => g.rule(at, Rule::Disfluency(Lang::Any)),
            ("vocabulary", "") => g.rule(at, Rule::Vocabulary),
            ("address", "") => g.rule(at, Rule::Address),
            ("continue", "") => g.rule(at, Rule::Continue),
            ("cleanup", "") => g.tidy(at),
            ("translate", pair) if !pair.is_empty() => {
                let (from, to) = pair.split_once(':').unwrap_or(("en", pair));
                g.ask(
                    at,
                    Request::translate(from, to),
                    Check::Usable { translation: true },
                )
            }
            _ if ProcessingChain::from_nick(step).is_some() => {
                return Err(format!(
                    "{step:?} is a preset: use it alone (--graph {step}), not in a list"
                ))
            }
            _ => return Err(format!("unknown step {step:?} (want {STEPS})")),
        };
    }
    Ok(g.finish(at))
}

/// Every ordered subset of the comma list `items`, shortest first: 3 items
/// are 3 + 6 + 6 graphs.
pub fn sweep(items: &str) -> Result<Vec<String>, String> {
    let items: Vec<&str> = items
        .split(',')
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .collect();
    if items.is_empty() {
        return Err("--sweep needs steps, e.g. --sweep disfluency,cleanup".into());
    }
    if items.len() > SWEEP_MAX {
        return Err(format!(
            "--sweep takes at most {SWEEP_MAX} steps (more is {} graphs)",
            (1..=items.len())
                .map(|k| permutations(items.len(), k))
                .sum::<usize>()
        ));
    }
    let mut specs = Vec::new();
    for len in 1..=items.len() {
        extend(&items, len, &mut Vec::new(), &mut specs);
    }
    // Fail on a bad step now, not after the speech.
    for spec in &specs {
        parse(spec)?;
    }
    Ok(specs)
}

fn permutations(n: usize, k: usize) -> usize {
    (n - k + 1..=n).product()
}

fn extend(items: &[&str], len: usize, taken: &mut Vec<usize>, out: &mut Vec<String>) {
    if taken.len() == len {
        out.push(
            taken
                .iter()
                .map(|&i| items[i])
                .collect::<Vec<_>>()
                .join(","),
        );
        return;
    }
    for i in 0..items.len() {
        if !taken.contains(&i) {
            taken.push(i);
            extend(items, len, taken, out);
            taken.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_id_is_the_daemons_preset() {
        for chain in ProcessingChain::ALL {
            let spec = parse(chain.nick()).unwrap();
            assert_eq!(spec.graph, Graph::preset(chain), "{}", chain.nick());
            assert_eq!(spec.label, chain.nick());
        }
    }

    #[test]
    fn a_list_of_steps_builds_the_graph_the_preset_is() {
        assert_eq!(
            parse("disfluency,vocabulary,cleanup").unwrap().graph,
            Graph::preset(ProcessingChain::TIDY)
        );
        assert_eq!(
            parse("disfluency,vocabulary,cleanup,translate=fr")
                .unwrap()
                .graph,
            Graph::preset(ProcessingChain::TIDY_FRENCH)
        );
        assert_eq!(
            parse("disfluency,vocabulary,cleanup,translate=en:fr")
                .unwrap()
                .graph,
            Graph::preset(ProcessingChain::TIDY_FRENCH)
        );
    }

    #[test]
    fn a_translation_names_its_languages() {
        let graph = parse("translate=de:es").unwrap().graph;
        assert_eq!(graph.requests(), [Request::translate("de", "es")]);
    }

    #[test]
    fn steps_are_compiled_in_their_order_like_the_daemon() {
        for list in ["tidy,translate=fr", " tidy , translate=FR "] {
            assert_eq!(
                parse_steps(list, None).unwrap().graph,
                Graph::preset(ProcessingChain::TIDY_FRENCH),
                "{list}"
            );
        }
        assert_ne!(
            parse_steps("translate=fr,tidy", None).unwrap().graph,
            Graph::preset(ProcessingChain::TIDY_FRENCH),
            "the order given is the order run"
        );
        let fr = parse_steps("tidy,translate=de", Some("fr")).unwrap();
        assert_eq!(fr.graph.requests(), [Request::translate("fr", "de")]);
        assert_eq!(fr.label, "steps: tidy,translate=de");
        let zh = parse_steps("script=simplified", Some("zh")).unwrap();
        assert_eq!(zh.script, ChineseScript::Simplified);
        assert_eq!(parse_steps("", None).unwrap().graph, Graph::passthrough());
        assert!(parse_steps("tidy,shout", None)
            .unwrap_err()
            .contains("shout"));
    }

    #[test]
    fn nothing_is_exact() {
        assert_eq!(parse("").unwrap().graph, Graph::passthrough());
        assert_eq!(parse("").unwrap().label, "exact");
    }

    #[test]
    fn a_script_is_a_stage_after_the_graph() {
        let spec = parse("script=traditional-taiwan").unwrap();
        assert_eq!(spec.graph, Graph::passthrough());
        assert_eq!(spec.script, ChineseScript::TraditionalTaiwan);
        let spec = parse("tidy,script=simplified").unwrap();
        assert_eq!(spec.graph, Graph::preset(ProcessingChain::TIDY));
        assert_eq!(spec.script, ChineseScript::Simplified);
        assert!(parse("script=klingon")
            .unwrap_err()
            .contains("traditional-taiwan"));
    }

    #[test]
    fn mistakes_are_named() {
        assert!(parse("disfluency,shout").unwrap_err().contains("\"shout\""));
        assert!(parse("translate").unwrap_err().contains("unknown step"));
        assert!(parse("disfluency,tidy").unwrap_err().contains("preset"));
    }

    #[test]
    fn a_sweep_is_every_ordered_subset_shortest_first() {
        let specs = sweep("disfluency,vocabulary,cleanup").unwrap();
        assert_eq!(specs.len(), 3 + 6 + 6);
        assert_eq!(&specs[..3], ["disfluency", "vocabulary", "cleanup"]);
        assert!(specs.contains(&"cleanup,disfluency".to_owned()));
        assert!(specs.contains(&"vocabulary,cleanup,disfluency".to_owned()));
        let mut unique = specs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), specs.len());
    }

    #[test]
    fn a_sweep_is_bounded_and_checked() {
        assert!(sweep("a,b,c,d,e,f").unwrap_err().contains("at most 5"));
        assert!(sweep("disfluency,shout").unwrap_err().contains("shout"));
        assert!(sweep(" , ").is_err());
    }
}
