//! Typed text and the solver that routes a processing chain through it.
//!
//! Pure logic: callers describe the implementations they have (installed
//! models, catalog entries, built-in converters) and [`solve`] says how each
//! step of a chain runs, inserting script converters and English pivots.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::fmt;

/// How a language is written. As a pattern, `Unspecified` accepts any script.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Script {
    Unspecified,
    /// Several scripts at once, or unknown where it matters (Chinese).
    Mixed,
    /// ISO 15924 with an optional region: `Hans`, `Hant`, `Hant-TW`.
    Known(String),
}

/// A language and the script it is written in.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Tongue {
    pub language: String,
    pub script: Script,
}

impl Tongue {
    pub fn new(language: &str, script: Script) -> Self {
        Self {
            language: language.to_owned(),
            script,
        }
    }

    /// `en`, `zh-Hant`, `zh-Hant-TW`: the first part is the language, any
    /// rest its script.
    pub fn parse(tag: &str) -> Self {
        match tag.split_once(['-', '_']) {
            Some((language, rest)) if !rest.is_empty() => {
                Self::new(language, Script::Known(rest.replace('_', "-")))
            }
            _ => Self::new(tag.trim_end_matches(['-', '_']), Script::Unspecified),
        }
    }

    /// Whether text of type `text` is acceptable where `self` is expected.
    pub fn accepts(&self, text: &Tongue) -> bool {
        self.language == text.language
            && (self.script == Script::Unspecified || self.script == text.script)
    }
}

impl fmt::Display for Tongue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.script {
            Script::Unspecified => write!(f, "{}", self.language),
            Script::Mixed => write!(f, "{} (mixed script)", self.language),
            Script::Known(s) => write!(f, "{}-{s}", self.language),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Same language and meaning, another script. May be inserted anywhere.
    Transcode,
    /// Another language. Used only inside a Translate step.
    Translate,
    /// Same type in and out.
    Tidy,
}

/// One way to carry out an operation.
#[derive(Clone, Debug)]
pub struct Implementation {
    pub id: String,
    pub kind: Kind,
    pub accepts: Tongue,
    /// Ignored for `Tidy`.
    pub produces: Tongue,
    /// False for a catalog entry that must be downloaded first.
    pub installed: bool,
    pub latency_ms: u32,
    /// 0 to 1000; breaks ties.
    pub quality: u32,
}

/// The scripts the built-in OpenCC converter writes Chinese in.
pub const OPENCC_SCRIPTS: [&str; 4] = ["Hans", "Hant", "Hant-TW", "Hant-HK"];

/// OpenCC as transcoders: text of any script in each of `languages`
/// becomes each of [`OPENCC_SCRIPTS`].
pub fn opencc<'a>(languages: impl IntoIterator<Item = &'a str>) -> Vec<Implementation> {
    let mut out = Vec::new();
    for language in languages {
        for script in OPENCC_SCRIPTS {
            out.push(Implementation {
                id: format!("opencc-{language}-{script}"),
                kind: Kind::Transcode,
                accepts: Tongue::new(language, Script::Unspecified),
                produces: Tongue::new(language, Script::Known(script.to_owned())),
                installed: true,
                latency_ms: 0,
                quality: 1000,
            });
        }
    }
    out
}

/// What the user put in the chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Tidy,
    /// The type to reach: `en`, `zh-Hans`.
    Translate {
        to: Tongue,
    },
    /// Write the language in this script.
    Script {
        to: Script,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hop {
    /// Index into the implementations given to [`solve`].
    pub implementation: usize,
    /// A converter added for a step that did not ask for one.
    pub inserted: bool,
    pub installed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    pub hops: Vec<Hop>,
    /// The type of the text after the step.
    pub produces: Tongue,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Runs(Path),
    /// Runs once the path's uninstalled hops are downloaded.
    Needs(Path),
    /// Leaves the text as it is, for this reason.
    Skipped(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub steps: Vec<Outcome>,
    /// The type of the text after the last step.
    pub output: Tongue,
}

/// Downloads, then model hops, then latency, then lost quality, then hops.
type Cost = (u32, u32, u32, u32, u32);

fn cost(imp: &Implementation) -> Cost {
    (
        u32::from(!imp.installed),
        u32::from(imp.kind != Kind::Transcode),
        imp.latency_ms,
        1000u32.saturating_sub(imp.quality),
        1,
    )
}

fn add(a: Cost, b: Cost) -> Cost {
    (a.0 + b.0, a.1 + b.1, a.2 + b.2, a.3 + b.3, a.4 + b.4)
}

type Found = (Cost, Vec<usize>, Tongue);

/// The cheapest run of allowed implementations from `start` to a type `goal`
/// accepts.
fn reach(
    impls: &[Implementation],
    start: &Tongue,
    translate: bool,
    goal: impl Fn(&Tongue) -> bool,
) -> Option<Found> {
    let mut heap = BinaryHeap::new();
    heap.push(Reverse(((0, 0, 0, 0, 0), start.clone(), Vec::new())));
    let mut seen = HashSet::new();
    while let Some(Reverse((c, at, path))) = heap.pop() {
        if !seen.insert(at.clone()) {
            continue;
        }
        if goal(&at) {
            return Some((c, path, at));
        }
        for (i, imp) in impls.iter().enumerate() {
            let usable = match imp.kind {
                Kind::Transcode => true,
                Kind::Translate => translate,
                Kind::Tidy => false,
            };
            if usable && imp.accepts.accepts(&at) && imp.produces != at {
                let mut next = path.clone();
                next.push(i);
                heap.push(Reverse((add(c, cost(imp)), imp.produces.clone(), next)));
            }
        }
    }
    None
}

fn run_step(impls: &[Implementation], step: &Step, at: &Tongue) -> Outcome {
    let found = match step {
        Step::Translate { to } => {
            if to.accepts(at) {
                return Outcome::Skipped(format!("the text is already {to}"));
            }
            reach(impls, at, true, |t| to.accepts(t))
        }
        Step::Script { to } => reach(impls, at, false, |t| {
            t.language == at.language && t.script == *to
        }),
        Step::Tidy => impls
            .iter()
            .enumerate()
            .filter(|(_, imp)| imp.kind == Kind::Tidy)
            .filter_map(|(i, tidy)| {
                let (c, mut path, end) = reach(impls, at, false, |t| tidy.accepts.accepts(t))?;
                path.push(i);
                Some((add(c, cost(tidy)), path, end))
            })
            .min_by_key(|(c, ..)| *c),
    };
    let Some((_, indices, end)) = found else {
        return Outcome::Skipped(match step {
            Step::Tidy => format!("no tidy model accepts {at} text"),
            Step::Translate { to } => format!("no model or converter turns {at} text into {to}"),
            Step::Script { to } => format!("no converter writes {at} text as {to:?}"),
        });
    };
    let hops = indices
        .into_iter()
        .map(|i| Hop {
            implementation: i,
            inserted: impls[i].kind == Kind::Transcode && !matches!(step, Step::Script { .. }),
            installed: impls[i].installed,
        })
        .collect::<Vec<_>>();
    let produces = match (step, hops.last()) {
        (Step::Tidy, _) | (_, None) => end,
        (_, Some(h)) => impls[h.implementation].produces.clone(),
    };
    let needs = hops.iter().any(|h| !h.installed);
    let path = Path { hops, produces };
    if needs {
        Outcome::Needs(path)
    } else {
        Outcome::Runs(path)
    }
}

/// How each step of `chain` runs on text of type `input`, in order. A
/// skipped step leaves the type unchanged.
pub fn solve(chain: &[Step], input: &Tongue, impls: &[Implementation]) -> Plan {
    let mut at = input.clone();
    let steps = chain
        .iter()
        .map(|step| {
            let outcome = run_step(impls, step, &at);
            if let Outcome::Runs(p) | Outcome::Needs(p) = &outcome {
                at = p.produces.clone();
            }
            outcome
        })
        .collect();
    Plan { steps, output: at }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(tag: &str) -> Tongue {
        Tongue::parse(tag)
    }

    fn imp(id: &str, kind: Kind, from: &str, to: &str, installed: bool) -> Implementation {
        Implementation {
            id: id.to_owned(),
            kind,
            accepts: t(from),
            produces: t(to),
            installed,
            latency_ms: 100,
            quality: 800,
        }
    }

    fn opencc() -> Vec<Implementation> {
        super::opencc(["zh"])
    }

    fn ids<'a>(plan: &Plan, i: usize, impls: &'a [Implementation]) -> Vec<&'a str> {
        match &plan.steps[i] {
            Outcome::Runs(p) | Outcome::Needs(p) => p
                .hops
                .iter()
                .map(|h| impls[h.implementation].id.as_str())
                .collect(),
            Outcome::Skipped(why) => panic!("skipped: {why}"),
        }
    }

    fn to(tag: &str) -> Vec<Step> {
        vec![Step::Translate { to: t(tag) }]
    }

    #[test]
    fn mixed_chinese_is_converted_for_the_traditional_model() {
        let mut impls = opencc();
        impls.push(imp("zh-en", Kind::Translate, "zh-Hant", "en", true));
        let mixed = Tongue::new("zh", Script::Mixed);
        let plan = solve(&to("en"), &mixed, &impls);
        assert_eq!(ids(&plan, 0, &impls), ["opencc-zh-Hant", "zh-en"]);
        assert!(
            matches!(&plan.steps[0], Outcome::Runs(p) if p.hops[0].inserted && !p.hops[1].inserted)
        );
        assert_eq!(plan.output, t("en"));
    }

    /// The catalog's measurements: zh-Hans-en 15 ms, chrF 0.582; zh-Hant-en
    /// 25 ms and not measured (no FLEURS slice).
    fn chinese_models(hans: bool, hant: bool) -> Vec<Implementation> {
        let mut impls = opencc();
        let mut zh_hans = imp("zh-Hans-en", Kind::Translate, "zh-Hans", "en", hans);
        (zh_hans.latency_ms, zh_hans.quality) = (15, 582);
        let mut zh_hant = imp("zh-Hant-en", Kind::Translate, "zh-Hant", "en", hant);
        (zh_hant.latency_ms, zh_hant.quality) = (25, 0);
        impls.extend([zh_hans, zh_hant]);
        impls
    }

    #[test]
    fn mixed_chinese_takes_the_simplified_model_when_both_are_installed() {
        let impls = chinese_models(true, true);
        let plan = solve(&to("en"), &Tongue::new("zh", Script::Mixed), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["opencc-zh-Hans", "zh-Hans-en"]);
        let plan = solve(&to("en"), &t("zh-Hans"), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["zh-Hans-en"]);
    }

    #[test]
    fn an_installed_traditional_model_wins_over_downloading_the_simplified_one() {
        let impls = chinese_models(false, true);
        let plan = solve(&to("en"), &t("zh-Hans"), &impls);
        assert!(matches!(plan.steps[0], Outcome::Runs(_)));
        assert_eq!(ids(&plan, 0, &impls), ["opencc-zh-Hant", "zh-Hant-en"]);
    }

    #[test]
    fn equal_cost_takes_the_fewest_hops() {
        let mut impls = opencc();
        impls.push(imp("zh-en", Kind::Translate, "zh-Hant", "en", true));
        let plan = solve(&to("zh-Hant"), &Tongue::new("zh", Script::Mixed), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["opencc-zh-Hant"]);
    }

    #[test]
    fn no_converter_is_inserted_when_the_text_already_fits() {
        let mut impls = opencc();
        impls.push(imp("zh-en", Kind::Translate, "zh-Hant", "en", true));
        let plan = solve(&to("en"), &t("zh-Hant"), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["zh-en"]);
    }

    #[test]
    fn farsi_reaches_french_through_english() {
        let impls = [
            imp("fa-en", Kind::Translate, "fa", "en", true),
            imp("en-fr", Kind::Translate, "en", "fr", true),
        ];
        let plan = solve(&to("fr"), &t("fa"), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["fa-en", "en-fr"]);
    }

    #[test]
    fn english_reaches_simplified_chinese_directly() {
        let mut impls = opencc();
        impls.push(imp("en-zh", Kind::Translate, "en", "zh-Hans", true));
        let plan = solve(&to("zh-Hans"), &t("en"), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["en-zh"]);
        assert_eq!(plan.output, t("zh-Hans"));
    }

    #[test]
    fn unreachable_target_names_what_is_missing() {
        let impls = [imp("fa-en", Kind::Translate, "fa", "en", true)];
        let plan = solve(&to("fr"), &t("fa"), &impls);
        let Outcome::Skipped(why) = &plan.steps[0] else {
            panic!("{plan:?}")
        };
        assert!(why.contains("fa") && why.contains("fr"), "{why}");
        assert_eq!(plan.output, t("fa"));
    }

    #[test]
    fn translation_is_never_used_for_another_step() {
        let impls = [
            imp("fr-en", Kind::Translate, "fr", "en", true),
            imp("tidy-en", Kind::Tidy, "en", "en", true),
        ];
        let plan = solve(&[Step::Tidy], &t("fr"), &impls);
        assert!(matches!(plan.steps[0], Outcome::Skipped(_)));
        assert_eq!(plan.output, t("fr"));
    }

    #[test]
    fn installed_models_win_over_downloads() {
        let impls = [
            imp("direct", Kind::Translate, "fa", "fr", false),
            imp("fa-en", Kind::Translate, "fa", "en", true),
            imp("en-fr", Kind::Translate, "en", "fr", true),
        ];
        let plan = solve(&to("fr"), &t("fa"), &impls);
        assert!(matches!(plan.steps[0], Outcome::Runs(_)));
        assert_eq!(ids(&plan, 0, &impls), ["fa-en", "en-fr"]);
    }

    #[test]
    fn fewest_downloads_then_fewest_hops() {
        let impls = [
            imp("fa-en", Kind::Translate, "fa", "en", false),
            imp("en-fr", Kind::Translate, "en", "fr", false),
            imp("direct", Kind::Translate, "fa", "fr", false),
        ];
        let plan = solve(&to("fr"), &t("fa"), &impls);
        assert!(matches!(plan.steps[0], Outcome::Needs(_)));
        assert_eq!(ids(&plan, 0, &impls), ["direct"]);
    }

    #[test]
    fn script_step_converts_and_the_next_step_sees_it() {
        let mut impls = opencc();
        impls.push(imp("zh-en", Kind::Translate, "zh-Hant", "en", true));
        let chain = [
            Step::Script {
                to: Script::Known("Hant".into()),
            },
            Step::Translate { to: t("en") },
        ];
        let plan = solve(&chain, &Tongue::new("zh", Script::Mixed), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["opencc-zh-Hant"]);
        assert_eq!(ids(&plan, 1, &impls), ["zh-en"]);
    }

    #[test]
    fn tidy_runs_on_text_it_accepts() {
        let impls = [imp("tidy-en", Kind::Tidy, "en", "en", true)];
        let plan = solve(&[Step::Tidy], &t("en"), &impls);
        assert_eq!(ids(&plan, 0, &impls), ["tidy-en"]);
    }
}
