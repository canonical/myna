//! How a chain runs, read from `myna_core::graph::solve`: the registry
//! Settings can plan with (installed models as installed, catalog entries
//! as downloads, OpenCC as built-in transcoders) and the per-step verdicts
//! the Processing page shows.

use myna_core::chains::{may_be_chinese, primary_language, ChainStep, KNOWN_STEPS};
use myna_core::graph::{self, Implementation, Kind, Outcome, Path, Script, Step, Tongue};
use myna_core::processing::ChineseScript;

use crate::plugin_catalog::Entry;
use crate::processing_steps::Offered;

const OPENCC_SCRIPTS: [&str; 4] = ["Hans", "Hant", "Hant-TW", "Hant-HK"];

/// What Settings can plan with.
pub struct World<'a> {
    impls: Vec<Implementation>,
    /// The catalog entry behind each implementation, parallel to `impls`.
    entries: Vec<Option<&'a Entry>>,
}

/// Why a step does not run where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Why {
    NoTarget,
    NoScript,
    AlreadyInTarget,
    /// Tidy reads this language only.
    OnlyFor(String),
    NotChinese,
    NoModel {
        from: String,
        to: String,
    },
}

/// What a step makes of the text where it stands in a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fit {
    /// Runs, reading text in `input` and leaving it in `output` (`None`
    /// when the language is not known). `inserted` names the converters
    /// the solver put before it.
    Runs {
        input: Option<String>,
        output: Option<String>,
        inserted: Vec<String>,
    },
    /// Skipped: the text there is not what it reads. `move_above` is the
    /// index of the earlier step that changed the language, when the step
    /// would fit above it.
    Skipped {
        why: Why,
        input: Option<String>,
        move_above: Option<usize>,
    },
    Off,
    /// An id this build does not know.
    Unknown,
}

/// The type of dictated text in `code`: Chinese comes in any script.
pub fn tongue_of(code: &str) -> Tongue {
    let language = primary_language(code);
    if may_be_chinese(Some(&language)) {
        Tongue::new("zh", Script::Mixed)
    } else {
        Tongue::new(&language, Script::Unspecified)
    }
}

fn param<'a>(step: &'a ChainStep, name: &str) -> Option<&'a str> {
    step.params
        .get(name)
        .map(String::as_str)
        .filter(|v| !v.is_empty())
}

/// The step as the solver takes it, or why its own settings leave it nothing to do.
fn wanted(step: &ChainStep) -> Result<Step, Why> {
    match step.step.as_str() {
        "translate" => param(step, "to")
            .map(|to| Step::Translate {
                to: Tongue::parse(to),
            })
            .ok_or(Why::NoTarget),
        "script" => {
            let known = |s: &str| Script::Known(s.to_owned());
            match param(step, "mode").and_then(ChineseScript::from_nick) {
                Some(ChineseScript::Simplified) => Ok(known("Hans")),
                Some(ChineseScript::Traditional) => Ok(known("Hant")),
                Some(ChineseScript::TraditionalTaiwan) => Ok(known("Hant-TW")),
                Some(ChineseScript::TraditionalHongKong) => Ok(known("Hant-HK")),
                Some(ChineseScript::AsRecognised) | None => Err(Why::NoScript),
            }
            .map(|to| Step::Script { to })
        }
        _ => Ok(Step::Tidy),
    }
}

fn written_first(script: &Script) -> String {
    use gettextrs::gettext;
    match script {
        Script::Known(s) if s == "Hans" => gettext("Writes Simplified first"),
        Script::Known(s) if s == "Hant" => gettext("Writes Traditional first"),
        Script::Known(s) if s == "Hant-TW" => gettext("Writes Traditional (Taiwan) first"),
        Script::Known(s) if s == "Hant-HK" => gettext("Writes Traditional (Hong Kong) first"),
        _ => gettext("Converts the script first"),
    }
}

impl<'a> World<'a> {
    /// Installed models are those `daemon` offers; every other catalog
    /// entry is a download.
    pub fn new(catalog: &'a [Entry], daemon: Option<&Offered>) -> Self {
        let mut world = Self {
            impls: Vec::new(),
            entries: Vec::new(),
        };
        let model = |id: String, kind, accepts, produces, installed| Implementation {
            id,
            kind,
            accepts,
            produces,
            installed,
            latency_ms: 100,
            quality: 800,
        };
        for script in OPENCC_SCRIPTS {
            world.impls.push(model(
                format!("opencc-{script}"),
                Kind::Transcode,
                Tongue::new("zh", Script::Unspecified),
                Tongue::new("zh", Script::Known(script.to_owned())),
                true,
            ));
            world.entries.push(None);
        }
        for entry in catalog {
            let kind = match entry.step.id.as_str() {
                "translate" => Kind::Translate,
                "tidy" => Kind::Tidy,
                _ => continue,
            };
            if entry.in_lang.is_empty() || entry.in_lang == "any" {
                continue;
            }
            let accepts = Tongue::parse(&entry.in_lang);
            let produces = match kind {
                Kind::Translate => Tongue::parse(&entry.out_lang),
                _ => accepts.clone(),
            };
            let installed = daemon.is_some_and(|d| d.offers(&entry.model));
            world
                .impls
                .push(model(entry.id.clone(), kind, accepts, produces, installed));
            world.entries.push(Some(entry));
        }
        for (from, to) in daemon.iter().flat_map(|d| &d.translations) {
            let (accepts, produces) = (Tongue::parse(from), Tongue::parse(to));
            let held = world.impls.iter().any(|i| {
                i.installed
                    && i.kind == Kind::Translate
                    && i.accepts == accepts
                    && i.produces == produces
            });
            if !held {
                world.impls.push(model(
                    format!("{from}>{to}"),
                    Kind::Translate,
                    accepts,
                    produces,
                    true,
                ));
                world.entries.push(None);
            }
        }
        world
    }

    /// `step` on text of type `at`: the path it takes, or why it is skipped.
    fn judge(&self, step: &ChainStep, at: &Tongue) -> Result<Path, Why> {
        let solved = wanted(step)?;
        if let Step::Translate { to } = &solved {
            if to.language == at.language {
                return Err(Why::AlreadyInTarget);
            }
        }
        let mut plan = graph::solve(std::slice::from_ref(&solved), at, &self.impls);
        match plan.steps.remove(0) {
            Outcome::Runs(path) | Outcome::Needs(path) => Ok(path),
            Outcome::Skipped(_) => Err(match solved {
                Step::Tidy => Why::OnlyFor(
                    self.impls
                        .iter()
                        .find(|i| i.kind == Kind::Tidy)
                        .map_or("en", |i| i.accepts.language.as_str())
                        .to_owned(),
                ),
                Step::Script { .. } => Why::NotChinese,
                Step::Translate { to } => Why::NoModel {
                    from: at.language.clone(),
                    to: to.language,
                },
            }),
        }
    }

    /// Why `step` would not run on text in `from` (a language code), when it would not.
    pub fn skip(&self, step: &ChainStep, from: &str) -> Option<Why> {
        if !KNOWN_STEPS.contains(&step.step.as_str()) {
            return None;
        }
        self.judge(step, &tongue_of(from)).err()
    }

    /// The catalog entries a translation from `from` to `to` runs through,
    /// installed or not; `None` when no path reaches it.
    pub fn route(&self, from: &str, to: &str) -> Option<Vec<&'a Entry>> {
        let step = ChainStep::new("translate").with("to", to);
        let path = self.judge(&step, &tongue_of(from)).ok()?;
        Some(
            path.hops
                .iter()
                .filter_map(|hop| self.entries[hop.implementation])
                .collect(),
        )
    }

    /// What each step of `steps` makes of the text, in order, for dictation
    /// in `language` (`None` when the speech model decides).
    pub fn fits(&self, steps: &[ChainStep], language: Option<&str>) -> Vec<Fit> {
        let mut at = language.map(tongue_of);
        let mut changed: Option<(usize, Option<Tongue>)> = None;
        steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                if !step.on {
                    return Fit::Off;
                }
                if !KNOWN_STEPS.contains(&step.step.as_str()) {
                    return Fit::Unknown;
                }
                let judged = match &at {
                    Some(here) => self.judge(step, here).map(|path| {
                        let inserted = path
                            .hops
                            .iter()
                            .filter(|hop| hop.inserted)
                            .map(|hop| {
                                written_first(&self.impls[hop.implementation].produces.script)
                            })
                            .collect();
                        (Some(path.produces), inserted)
                    }),
                    None => wanted(step).map(|solved| match solved {
                        Step::Translate { to } => (Some(to), Vec::new()),
                        _ => (None, Vec::new()),
                    }),
                };
                let input = at.as_ref().map(|t| t.language.clone());
                match judged {
                    Ok((produced, inserted)) => {
                        if step.step == "translate" {
                            changed = Some((index, at.clone()));
                        }
                        at = produced.or(at.take());
                        Fit::Runs {
                            input,
                            output: at.as_ref().map(|t| t.language.clone()),
                            inserted,
                        }
                    }
                    Err(why) => {
                        let move_above = changed
                            .as_ref()
                            .filter(|(_, before)| match before {
                                Some(before) => self.judge(step, before).is_ok(),
                                None => wanted(step).is_ok(),
                            })
                            .map(|(at, _)| *at);
                        Fit::Skipped {
                            why,
                            input,
                            move_above,
                        }
                    }
                }
            })
            .collect()
    }
}

/// A skip in the user's words.
pub fn why_text(why: &Why) -> String {
    use gettextrs::gettext;
    match why {
        Why::NoTarget => gettext("Needs a language to translate to"),
        Why::NoScript => gettext("Needs a script to write"),
        Why::AlreadyInTarget => gettext("The text is already in that language"),
        Why::NotChinese => gettext("Applies when the text is Chinese"),
        Why::OnlyFor(language) => {
            // TRANSLATORS: {language} is a language name, such as "English".
            gettext("Applies when the text is {language}")
                .replace("{language}", &crate::languages::language_in_text(language))
        }
        Why::NoModel { from, to } => {
            // TRANSLATORS: {from} and {to} are language names, such as "فارسی" and "Français".
            gettext("No model translates {from} to {to} yet")
                .replace("{from}", &crate::languages::language_in_text(from))
                .replace("{to}", &crate::languages::language_in_text(to))
        }
    }
}
