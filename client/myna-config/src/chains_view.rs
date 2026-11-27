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
            let (badge, skipped, subtitle, fix, (inserted, inserted_after)) = match &fit {
                Fit::Runs {
                    input,
                    output,
                    before,
                    after,
                } => (
                    written(step, input.as_deref(), output.as_deref()),
                    false,
                    row.subtitle.clone(),
                    None,
                    (before.join(", "), after.join(", ")),
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
                        (String::new(), String::new()),
                    )
                }
                Fit::Off => (
                    String::new(),
                    false,
                    row.description.clone(),
                    None,
                    (String::new(), String::new()),
                ),
                Fit::Unknown => (
                    String::new(),
                    true,
                    gettextrs::gettext("Not known to this version of Myna"),
                    None,
                    (String::new(), String::new()),
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
                inserted_after,
            };
            row.subtitle = subtitle;
            row
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
}

#[cfg(test)]
mod chain_tests {
    use super::*;

    fn pair(from: &str, to: &str) -> Entry {
        let id = format!("{}-{}", from.to_lowercase(), to.to_lowercase());
        serde_json::from_value(serde_json::json!({
            "id": id, "snap": "myna-translation", "component": format!("model-translate-{id}"),
            "model": format!("m-{id}"), "task": "text.translate",
            "in_lang": from, "out_lang": to,
            "download_bytes": 30_000_000,
            "step": {"id": "translate", "stage": "translate", "label_key": "step.translate.name",
                     "description_key": "", "example_key": ""},
        }))
        .unwrap()
    }

    /// What Charles' laptop held: en to fr and zh-Hant to en installed, en to
    /// zh-Hans and zh-Hans to en only in the catalog.
    fn laptop() -> (Vec<Entry>, Offered) {
        let catalog = vec![
            pair("en", "fr"),
            pair("zh-Hant", "en"),
            pair("en", "zh-Hans"),
            pair("zh-Hans", "en"),
        ];
        let offered = Offered {
            models: vec!["m-en-fr".into(), "m-zh-hant-en".into()],
            ..Offered::default()
        };
        (catalog, offered)
    }

    fn chain() -> Chain {
        Chain {
            id: "c".into(),
            name: "c".into(),
            steps: vec![
                ChainStep::new(TRANSLATE).with("to", "zh-Hans"),
                ChainStep::new(TRANSLATE).with("to", "fr"),
            ],
        }
    }

    fn lines_with(
        installing: &BTreeMap<String, Option<u8>>,
        preparing: &BTreeSet<String>,
    ) -> Vec<Row> {
        let (catalog, offered) = laptop();
        lines(
            &chain(),
            &Context {
                language: Some("en"),
                catalog: &catalog,
                daemon: Some(&offered),
                installing,
                preparing,
                locales: &[],
            },
        )
    }

    #[test]
    fn two_translate_steps_have_their_own_install_keys() {
        let rows = lines_with(&BTreeMap::new(), &BTreeSet::new());
        assert_eq!(rows[0].key, "translate:en>zh-Hans");
        assert_eq!(rows[1].key, "translate:zh>fr");
    }

    #[test]
    fn installing_one_translate_step_leaves_the_other_alone() {
        let rows = lines_with(&BTreeMap::new(), &BTreeSet::new());
        assert!(matches!(rows[0].standing, Standing::Missing(_)));
        let first = rows[0].key.clone();
        let installing = BTreeMap::from([(first.clone(), Some(40))]);
        let rows = lines_with(&installing, &BTreeSet::new());
        assert_eq!(rows[0].standing, Standing::Installing(Some(40)));
        assert!(
            !matches!(rows[1].standing, Standing::Installing(_)),
            "{:?}",
            rows[1].standing
        );
        let rows = lines_with(&BTreeMap::new(), &BTreeSet::from([first]));
        assert_eq!(rows[0].standing, Standing::Preparing);
        assert!(
            !matches!(rows[1].standing, Standing::Preparing),
            "{:?}",
            rows[1].standing
        );
    }
}
