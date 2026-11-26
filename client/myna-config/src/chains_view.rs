//! What the Processing page shows of the saved chains, and the editor's
//! ordered list, as plain values; `processing_ui` binds them to widgets.
//!
//! A step's place in its chain (the language it reads, whether it is
//! skipped there and which step to move it above) is
//! `myna_core::chains::fits`, the daemon's own judgement. What a step needs
//! installed comes from `processing_steps`, asked once per step with the
//! language the text is in at that step.

use std::collections::{BTreeMap, BTreeSet};

use myna_core::chains::{fits, Chain, ChainStep, Fit, Misfit};
use myna_core::processing::{param_key, Params};

use crate::plugin_catalog::Entry;
use crate::processing_steps::{routable, rows, Inputs, Offered, Row, Source, Standing, TRANSLATE};

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
}

/// One step of the chain being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub index: usize,
    pub id: String,
    pub title: String,
    /// The language read and left: "EN → FR", "EN", or empty.
    pub badge: String,
    pub on: bool,
    /// What is wrong, else what the step does.
    pub subtitle: String,
    /// The step to move this one above to make it run, and its title.
    pub fix: Option<(usize, String)>,
    /// What the step needs, its install's progress, or ready.
    pub standing: Standing,
    /// The Translate target picker and the script picker.
    pub row: Row,
    pub skipped: bool,
    /// Which moves keep every translation of the chain doable.
    pub moves: Moves,
}

/// The moves of a step that leave no translation without a model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    pub up: bool,
    pub down: bool,
    pub top: bool,
    pub bottom: bool,
}

/// How many translations of `steps` that run no model can do, installed
/// or offered: the text reaches them in a language nothing translates from
/// into their target.
fn stranded(steps: &[ChainStep], ctx: &Context) -> usize {
    steps
        .iter()
        .zip(fits(steps, ctx.language))
        .filter(|(step, fit)| {
            step.step == TRANSLATE
                && matches!(fit, Fit::Runs { input: Some(from), output: Some(to) }
                    if !routable(ctx.catalog, ctx.daemon, from, to))
        })
        .count()
}

/// Whether turning `before` into `after` leaves a translation no model can
/// do that `before` did not have. The editor offers no such change, so a
/// chain is not built into a pair nothing translates.
pub fn strands(before: &[ChainStep], after: &[ChainStep], ctx: &Context) -> bool {
    stranded(after, ctx) > stranded(before, ctx)
}

/// The language the text is in just before position `at` of `steps`, as
/// `Source`: where a step inserted there would read it.
pub fn language_at(steps: &[ChainStep], at: usize, language: Option<&str>) -> Source {
    let before = &steps[..at.min(steps.len())];
    fits(before, language)
        .into_iter()
        .rev()
        .find_map(|fit| match fit {
            Fit::Runs { output, .. } => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| language.map(myna_core::chains::primary_language))
        .map_or(Source::Unknown, Source::One)
}

/// `steps` with step `index` translating to `to`.
fn retargeted(steps: &[ChainStep], index: usize, to: &str) -> Vec<ChainStep> {
    let mut steps = steps.to_vec();
    steps[index].params.insert("to".into(), to.to_owned());
    steps
}

/// A step's parameters in the `<step>.<param>` form `processing_steps` reads.
fn flat(step: &ChainStep) -> Params {
    step.params
        .iter()
        .map(|(name, value)| (param_key(&step.step, name), value.clone()))
        .collect()
}

/// `processing_steps`' row for `step`, judged with the text in `input`.
fn row_of(step: &ChainStep, input: Option<&str>, ctx: &Context) -> Row {
    let source = input.map_or(Source::Unknown, |l| Source::One(l.to_owned()));
    let params = flat(step);
    let enabled = [step.step.clone()];
    rows(&Inputs {
        language_unset: ctx.language.is_none(),
        enabled: &enabled,
        params: &params,
        source: &source,
        catalog: ctx.catalog,
        daemon: ctx.daemon,
        installing: ctx.installing,
        preparing: ctx.preparing,
        locales: ctx.locales,
    })
    .into_iter()
    .find(|row| row.id == step.step)
    .unwrap_or_else(|| Row {
        id: step.step.clone(),
        title: step.step.clone(),
        description: String::new(),
        enabled: step.on,
        standing: Standing::Ready,
        translate: None,
        script: None,
    })
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

/// Why a step is skipped, in the user's words.
pub fn misfit_text(why: Misfit) -> String {
    match why {
        Misfit::NotEnglish => gettextrs::gettext("Applies when the text is English"),
        Misfit::NoTarget => gettextrs::gettext("Needs a language to translate to"),
        Misfit::AlreadyInTarget => gettextrs::gettext("The text is already in that language"),
        Misfit::NotChinese => gettextrs::gettext("Applies when the text is Chinese"),
        Misfit::NoScript => gettextrs::gettext("Needs a script to write"),
    }
}

/// The editor's list for `chain`, in its order.
pub fn lines(chain: &Chain, ctx: &Context) -> Vec<Line> {
    let fitted = fits(&chain.steps, ctx.language);
    let rows: Vec<Row> = chain
        .steps
        .iter()
        .zip(&fitted)
        .map(|(step, fit)| {
            let input = match fit {
                Fit::Runs { input, .. } | Fit::Skipped { input, .. } => input.as_deref(),
                Fit::Off | Fit::Unknown => ctx.language,
            };
            row_of(step, input, ctx)
        })
        .collect();
    chain
        .steps
        .iter()
        .zip(fitted)
        .zip(&rows)
        .enumerate()
        .map(|(index, ((step, fit), row))| {
            let (badge, skipped, subtitle, fix) = match &fit {
                Fit::Runs { input, output } => (
                    badge(input.as_deref(), output.as_deref()),
                    false,
                    row.subtitle(),
                    None,
                ),
                Fit::Skipped {
                    why,
                    input,
                    move_above,
                } => {
                    // TRANSLATORS: why a step of a chain does not run where it is, such as "Skipped here: Applies when the text is English".
                    let frame = gettextrs::gettext("Skipped here: {why}");
                    let fix = move_above.map(|at| (at, rows[at].title.clone()));
                    (
                        badge(input.as_deref(), input.as_deref()),
                        true,
                        frame.replace("{why}", &misfit_text(*why)),
                        fix,
                    )
                }
                Fit::Off => (String::new(), false, row.description.clone(), None),
                Fit::Unknown => (
                    String::new(),
                    true,
                    gettextrs::gettext("Not known to this version of Myna"),
                    None,
                ),
            };
            let keeps = |to: usize| {
                to == index || !strands(&chain.steps, &moved(&chain.steps, index, to), ctx)
            };
            let last = chain.steps.len().saturating_sub(1);
            let moves = Moves {
                up: index > 0 && keeps(index - 1),
                down: index < last && keeps(index + 1),
                top: index > 0 && keeps(0),
                bottom: index < last && keeps(last),
            };
            let fix = fix.filter(|(at, _)| keeps(*at));
            let mut row = row.clone();
            if let Some(translate) = row.translate.as_mut() {
                let chosen = translate
                    .selected
                    .map(|i| translate.targets[i].code.clone());
                translate.targets.retain(|target| {
                    Some(&target.code) == chosen.as_ref()
                        || !strands(
                            &chain.steps,
                            &retargeted(&chain.steps, index, &target.code),
                            ctx,
                        )
                });
                translate.selected =
                    chosen.and_then(|code| translate.targets.iter().position(|t| t.code == code));
            }
            Line {
                index,
                id: step.step.clone(),
                title: row.title.clone(),
                badge,
                on: step.on,
                subtitle,
                fix,
                standing: row.standing.clone(),
                row,
                skipped,
                moves,
            }
        })
        .collect()
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
                    if line.skipped {
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
                .filter(|line| line.on && !line.skipped)
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

/// The step the catalog's `item` adds: its step id, the target of a
/// translation model, the desktop's script for the Chinese script step.
pub fn step_of(item: &crate::step_catalog::Item, locales: &[String]) -> ChainStep {
    let step = ChainStep::new(&item.step);
    if let Some(to) = &item.target {
        return step.with("to", to);
    }
    if item.step == crate::processing_steps::SCRIPT {
        let script = crate::processing_steps::default_script(locales);
        return step.with("mode", script.nick());
    }
    step
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_at_a_place_is_what_the_steps_before_it_made() {
        let steps = vec![
            ChainStep::new("tidy"),
            ChainStep::new("translate").with("to", "fa"),
            ChainStep::new("translate").with("to", "fa"),
        ];
        let at = |n, language| language_at(&steps, n, language);
        assert_eq!(at(0, Some("en-GB")), Source::One("en".into()));
        assert_eq!(at(1, Some("en")), Source::One("en".into()));
        assert_eq!(at(2, Some("en")), Source::One("fa".into()));
        assert_eq!(at(3, Some("en")), Source::One("fa".into()));
        assert_eq!(at(9, Some("en")), Source::One("fa".into()));
        assert_eq!(at(1, None), Source::Unknown);
    }

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
}
