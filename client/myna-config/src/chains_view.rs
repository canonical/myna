//! What the Processing page shows of the saved chains, and the editor's
//! ordered list, as plain values; `processing_ui` binds them to widgets.
//!
//! A step's place in its chain (the language it reads, whether it is
//! skipped there and which step to move it above) is read from
//! `myna_core::graph::solve` through `chain_solve`. What a step needs
//! installed comes from `processing_steps`, asked once per step with the
//! language the text is in at that step.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::chains::{Chain, ChainStep};

use crate::chain_solve::{why_text, Fit, Why, World};
use crate::languages::Source;
use crate::plugin_catalog::Entry;
use crate::processing_steps::{self as steps, Moves, Offered, Place, Row, Standing, TRANSLATE};
use crate::step_catalog::{self, FRENCH, SIMPLIFIED, TRADITIONAL};
use myna_core::graph::Script;

/// What every step judgement needs besides the chain.
pub struct Context<'a> {
    /// The dictation language; `None` when the speech model decides.
    pub language: Option<&'a str>,
    pub catalog: &'a [Entry],
    pub daemon: Option<&'a Offered>,
    pub installing: &'a BTreeMap<String, Option<u8>>,
    pub preparing: &'a BTreeSet<String>,
    pub locales: &'a [String],
}

impl Context<'_> {
    /// The row builder's view of this context, for text in `source`.
    fn inputs<'a>(&'a self, source: &'a Source) -> steps::Inputs<'a> {
        steps::Inputs {
            language_unset: self.language.is_none(),
            source,
            catalog: self.catalog,
            daemon: self.daemon,
            installing: self.installing,
            preparing: self.preparing,
            locales: self.locales,
        }
    }
}

/// One saved chain on the Processing page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub id: String,
    pub name: String,
    pub active: bool,
    /// Its steps in order, a skipped one marked: "Tidy up speech → Translate".
    pub steps: String,
    /// What it needs installed before every step runs, if anything.
    pub needs: Option<String>,
    /// What it types for its sample, in a new paragraph.
    pub result: String,
}

/// How many translations of `steps` no model, installed or offered, can do.
fn stranded(steps: &[ChainStep], language: Option<&str>, world: &World) -> usize {
    world
        .fits(steps, language)
        .iter()
        .filter(|fit| {
            matches!(
                fit,
                Fit::Skipped {
                    why: Why::NoModel { .. },
                    ..
                }
            )
        })
        .count()
}

/// Whether turning `before` into `after` leaves a translation no model can
/// do that `before` did not have. The editor offers no such change, so a
/// chain is not built into a pair nothing translates.
pub fn strands(before: &[ChainStep], after: &[ChainStep], ctx: &Context) -> bool {
    let world = World::new(ctx.catalog, ctx.daemon);
    stranded_by(before, after, ctx.language, &world)
}

fn stranded_by(
    before: &[ChainStep],
    after: &[ChainStep],
    language: Option<&str>,
    world: &World,
) -> bool {
    stranded(after, language, world) > stranded(before, language, world)
}

/// The language the text is in just before position `at` of `steps`, as
/// `Source`: where a step inserted there would read it.
pub fn language_at(steps: &[ChainStep], at: usize, ctx: &Context) -> Source {
    let before = &steps[..at.min(steps.len())];
    World::new(ctx.catalog, ctx.daemon)
        .fits(before, ctx.language)
        .into_iter()
        .rev()
        .find_map(|fit| match fit {
            Fit::Runs { output, .. } => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| ctx.language.map(myna_core::chains::primary_language))
        .map_or(Source::Unknown, Source::One)
}

/// `steps` with step `index` translating to `to`.
fn retargeted(steps: &[ChainStep], index: usize, to: &str) -> Vec<ChainStep> {
    let mut steps = steps.to_vec();
    steps[index].params.insert("to".into(), to.to_owned());
    steps
}

/// The badge beside a step: the language it reads, and for a translation the
/// one it writes. The picker that names the target is the row under the step,
/// so the arrow says where it goes: "EN \u{2192} ZH-HANS", not a dangling "EN \u{2192}".
fn written(step: &ChainStep, input: Option<&str>, output: Option<&str>) -> String {
    // A code, not a name: "ZH" for either Chinese script, which the picker
    // beside it names.
    let to = (step.step == TRANSLATE)
        .then(|| {
            step.params
                .get("to")
                .map(|to| myna_core::chains::primary_language(to))
        })
        .flatten();
    match (input, to.as_deref()) {
        (Some(read), Some(to)) if read != to => {
            format!("{} \u{2192} {}", read.to_uppercase(), to.to_uppercase())
        }
        _ => badge(input, output),
    }
}

/// The language a step reads, read-only: the graph deduces it. A step that
/// changes the language ends in an arrow, its target being the picker
/// beside it ("EN →" then "Italiano").
fn badge(input: Option<&str>, output: Option<&str>) -> String {
    match (input, output) {
        (Some(i), Some(o)) if i != o => format!("{} \u{2192}", i.to_uppercase()),
        (Some(i), _) => i.to_uppercase(),
        _ => String::new(),
    }
}

/// The editor's list for `chain`, in its order: each step as `processing_steps`
/// judges it where it stands, with what the editor adds about its place.
pub fn lines(chain: &Chain, ctx: &Context) -> Vec<Row> {
    let world = World::new(ctx.catalog, ctx.daemon);
    let fitted = world.fits(&chain.steps, ctx.language);
    let rows: Vec<Row> = chain
        .steps
        .iter()
        .zip(&fitted)
        .map(|(step, fit)| {
            let input = match fit {
                Fit::Runs { input, .. } | Fit::Skipped { input, .. } => input.as_deref(),
                Fit::Off | Fit::Unknown => ctx.language,
            };
            let source = input.map_or(Source::Unknown, |l| Source::One(l.to_owned()));
            steps::row(step, &source, &ctx.inputs(&source))
        })
        .collect();
    let titles: Vec<String> = rows.iter().map(|row| row.title.clone()).collect();
    chain
        .steps
        .iter()
        .zip(fitted)
        .zip(rows)
        .enumerate()
        .map(|(index, ((step, fit), mut row))| {
            let (badge, skipped, subtitle, fix, inserted) = match &fit {
                Fit::Runs {
                    input,
                    output,
                    inserted,
                    ..
                } => (
                    written(step, input.as_deref(), output.as_deref()),
                    false,
                    row.subtitle.clone(),
                    None,
                    inserted.join(", "),
                ),
                Fit::Skipped {
                    why,
                    input,
                    move_above,
                } => {
                    // The row's "Skipped" badge says that it is; the subtitle says why.
                    let fix = move_above.map(|at| (at, titles[at].clone()));
                    (
                        badge(input.as_deref(), input.as_deref()),
                        true,
                        why_text(why),
                        fix,
                        String::new(),
                    )
                }
                Fit::Off => (
                    String::new(),
                    false,
                    row.description.clone(),
                    None,
                    String::new(),
                ),
                Fit::Unknown => (
                    String::new(),
                    true,
                    gettextrs::gettext("Not known to this version of Myna"),
                    None,
                    String::new(),
                ),
            };
            let keeps = |to: usize| {
                to == index
                    || !stranded_by(
                        &chain.steps,
                        &moved(&chain.steps, index, to),
                        ctx.language,
                        &world,
                    )
            };
            let last = chain.steps.len().saturating_sub(1);
            let moves = Moves {
                up: index > 0 && keeps(index - 1),
                down: index < last && keeps(index + 1),
                top: index > 0 && keeps(0),
                bottom: index < last && keeps(last),
            };
            let fix = fix.filter(|(at, _)| keeps(*at));
            if let Some(translate) = row.translate.as_mut() {
                let chosen = translate
                    .selected
                    .map(|i| translate.targets[i].code.clone());
                translate.targets.retain(|target| {
                    Some(&target.code) == chosen.as_ref()
                        || !stranded_by(
                            &chain.steps,
                            &retargeted(&chain.steps, index, &target.code),
                            ctx.language,
                            &world,
                        )
                });
                translate.selected =
                    chosen.and_then(|code| translate.targets.iter().position(|t| t.code == code));
            }
            row.place = Place {
                index,
                badge,
                fix,
                skipped,
                moves,
                inserted,
            };
            row.subtitle = subtitle;
            row
        })
        .collect()
}

/// What the text is typed into, for Try it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Field {
    #[default]
    NewParagraph,
    MidSentence,
    Address,
}

impl Field {
    pub const ALL: [Field; 3] = [Field::NewParagraph, Field::MidSentence, Field::Address];

    pub fn label(self) -> String {
        match self {
            Field::NewParagraph => gettextrs::gettext("A new paragraph"),
            Field::MidSentence => gettextrs::gettext("The middle of a sentence"),
            Field::Address => gettextrs::gettext("An address field"),
        }
    }

    /// The sample as the field takes it. The daemon's own fit reads the
    /// field; the samples only need the capital and the closing mark.
    fn fit(self, text: &str) -> String {
        let lower = |text: &str| {
            let mut chars = text.chars();
            chars
                .next()
                .map(|c| c.to_lowercase().chain(chars).collect())
                .unwrap_or_default()
        };
        match self {
            Field::NewParagraph => text.to_owned(),
            Field::MidSentence => lower(text),
            Field::Address => lower(text.trim_end_matches(['.', '!', '?'])),
        }
    }
}

/// One line of Try it's step by step: what ran and the sample after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceLine {
    pub title: String,
    pub text: String,
    /// Why the sample did not change, when it is not simply that the step skipped.
    pub note: Option<String>,
}

/// A chain run on a static sample, no model called.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    pub heard: String,
    pub typed: String,
    pub lines: Vec<TraceLine>,
}

fn script_sample(script: &Script) -> &'static str {
    match script {
        Script::Known(s) if s == "Hans" => SIMPLIFIED,
        _ => TRADITIONAL,
    }
}

/// `chain` on the catalog's own examples, in its order: the steps the solver
/// skips are marked and leave the sample alone, the converters it inserts
/// are lines of their own, and the field's fit comes last.
pub fn trace(chain: &Chain, field: Field, ctx: &Context) -> Trace {
    let rows = lines(chain, ctx);
    let fitted = World::new(ctx.catalog, ctx.daemon).fits(&chain.steps, ctx.language);
    let chinese = ctx
        .language
        .is_some_and(|l| myna_core::chains::primary_language(l) == "zh");
    let heard = if chinese {
        SIMPLIFIED.to_owned()
    } else {
        step_catalog::said()
    };
    let mut text = heard.clone();
    let mut out: Vec<TraceLine> = Vec::new();
    for ((step, row), fit) in chain.steps.iter().zip(&rows).zip(&fitted) {
        let mut note = None;
        let title = match fit {
            Fit::Off => {
                // TRANSLATORS: a step of a chain that is switched off; {step} is its name.
                gettextrs::gettext("{step} (off)").replace("{step}", &row.title)
            }
            Fit::Unknown => {
                note = Some(row.subtitle.clone());
                row.title.clone()
            }
            Fit::Skipped { why, .. } => {
                note = Some(why_text(why));
                gettextrs::gettext("{step} (skipped)").replace("{step}", &row.title)
            }
            Fit::Runs {
                input,
                output,
                inserted,
                converts,
            } => {
                for label in inserted {
                    if let Some(script) = converts.as_ref().filter(|_| *label != pivot()) {
                        if text == SIMPLIFIED || text == TRADITIONAL {
                            text = script_sample(script).to_owned();
                        }
                    }
                    out.push(TraceLine {
                        title: label.clone(),
                        text: text.clone(),
                        note: None,
                    });
                }
                match step.step.as_str() {
                    "tidy" if text == step_catalog::said() => text = step_catalog::tidied(),
                    "script" if text == SIMPLIFIED || text == TRADITIONAL => {
                        let simplified =
                            step.params.get("mode").map(String::as_str) == Some("simplified");
                        text = if simplified { SIMPLIFIED } else { TRADITIONAL }.to_owned();
                    }
                    "translate" => {
                        let pair = (
                            input.as_deref().unwrap_or("en"),
                            output.as_deref().unwrap_or_default(),
                        );
                        match pair {
                            ("en", "fr") => text = FRENCH.to_owned(),
                            ("zh", "en") => text = "software".to_owned(),
                            _ => {
                                note = Some(gettextrs::gettext("No example for this language yet"))
                            }
                        }
                    }
                    _ => {}
                }
                row.title.clone()
            }
        };
        out.push(TraceLine {
            title,
            text: text.clone(),
            note,
        });
    }
    let typed = field.fit(&text);
    if typed != text {
        out.push(TraceLine {
            title: gettextrs::gettext("Fitted to the field"),
            text: typed.clone(),
            note: Some(field.label()),
        });
    }
    Trace {
        heard,
        typed,
        lines: out,
    }
}

/// The inserted line that is a pivot through English, not a script converter.
fn pivot() -> String {
    gettextrs::gettext("Through English")
}

/// Every saved chain, for the page's list.
pub fn cards(chains: &[Chain], active: Option<&str>, ctx: &Context) -> Vec<Card> {
    chains
        .iter()
        .map(|chain| {
            let lines = lines(chain, ctx);
            let shown: Vec<String> = lines
                .iter()
                .filter(|line| line.on)
                .map(|line| {
                    if line.place.skipped {
                        // TRANSLATORS: a step of a chain that does not run where it stands.
                        gettextrs::gettext("{step} (skipped)").replace("{step}", &line.title)
                    } else {
                        line.title.clone()
                    }
                })
                .collect();
            let steps = if shown.is_empty() {
                gettextrs::gettext("No steps")
            } else {
                shown.join(" \u{2192} ")
            };
            let needs = lines
                .iter()
                .filter(|line| line.on && !line.place.skipped)
                .find_map(|line| match &line.standing {
                    // TRANSLATORS: {what} names a model to install, such as "the French model".
                    Standing::Missing(need) => {
                        Some(gettextrs::gettext("Needs {what}").replace("{what}", &need.what))
                    }
                    Standing::Unavailable(reason) => Some(reason.clone()),
                    _ => None,
                });
            Card {
                id: chain.id.clone(),
                name: chain.name.clone(),
                active: active == Some(chain.id.as_str()),
                steps,
                needs,
                result: trace(chain, Field::NewParagraph, ctx).typed,
            }
        })
        .collect()
}

/// `steps` with the one at `from` moved to `to`.
pub fn moved(steps: &[ChainStep], from: usize, to: usize) -> Vec<ChainStep> {
    let mut steps = steps.to_vec();
    if from < steps.len() && to < steps.len() {
        let step = steps.remove(from);
        steps.insert(to, step);
    }
    steps
}

/// A name for a new chain that no saved chain has: "New chain", then
/// "New chain 2".
pub fn new_name(chains: &[Chain]) -> String {
    let base = gettextrs::gettext("New chain");
    (1..)
        .map(|n| match n {
            1 => base.clone(),
            n => format!("{base} {n}"),
        })
        .find(|name| chains.iter().all(|chain| chain.name != *name))
        .expect("an unbounded range has a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_and_naming() {
        let steps = vec![
            ChainStep::new("a"),
            ChainStep::new("b"),
            ChainStep::new("c"),
        ];
        let ids = |steps: &[ChainStep]| steps.iter().map(|s| s.step.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&moved(&steps, 2, 0)), ["c", "a", "b"]);
        assert_eq!(ids(&moved(&steps, 0, 2)), ["b", "c", "a"]);
        assert_eq!(ids(&moved(&steps, 0, 9)), ["a", "b", "c"]);
        assert_eq!(
            ids(&moved(&steps, 0, 3)),
            ["a", "b", "c"],
            "one past the end"
        );
        let named = |name: &str| Chain {
            id: name.into(),
            name: name.into(),
            steps: Vec::new(),
        };
        assert_eq!(new_name(&[]), "New chain");
        assert_eq!(new_name(&[named("New chain")]), "New chain 2");
    }

    #[test]
    fn the_field_has_the_last_word_on_a_sample() {
        let said = "So I think we should go.";
        assert_eq!(Field::NewParagraph.fit(said), said);
        assert_eq!(Field::MidSentence.fit(said), "so I think we should go.");
        assert_eq!(Field::Address.fit(said), "so I think we should go");
    }

    #[test]
    fn a_trace_follows_the_chain_and_marks_what_does_not_run() {
        let (installing, preparing) = (BTreeMap::new(), BTreeSet::new());
        let ctx = Context {
            language: Some("zh"),
            catalog: &[],
            daemon: None,
            installing: &installing,
            preparing: &preparing,
            locales: &[],
        };
        let chain = |steps: Vec<ChainStep>| Chain {
            id: "c".into(),
            name: "c".into(),
            steps,
        };
        let none = trace(&chain(Vec::new()), Field::NewParagraph, &ctx);
        assert_eq!(none.heard, SIMPLIFIED);
        assert_eq!(none.typed, SIMPLIFIED);
        assert!(none.lines.is_empty());
        let script = ChainStep::new("script").with("mode", "traditional");
        let ran = trace(&chain(vec![script.clone()]), Field::NewParagraph, &ctx);
        assert_eq!(ran.typed, TRADITIONAL);
        assert_eq!(ran.lines.len(), 1);
        let mut off = script;
        off.on = false;
        let paused = trace(&chain(vec![off]), Field::NewParagraph, &ctx);
        assert_eq!(paused.typed, SIMPLIFIED);
        assert!(paused.lines[0].title.ends_with("(off)"));
    }
}
