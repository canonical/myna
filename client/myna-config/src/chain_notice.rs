//! What the daemon says about whether the chosen processing chain can do all
//! it asks, in the plain words the Processing tab and Diagnostics show.

/// The daemon's `ChainStatus`, with `ChainMissing` for the degraded case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChainState {
    Ready,
    /// These tasks (`text.translate`) have no installed model.
    Degraded(Vec<String>),
    /// A model was connected after the daemon started.
    Restart,
}

impl ChainState {
    /// `None` for a daemon too old to publish the properties (empty status)
    /// and for a value this Settings does not know: say nothing rather than
    /// guess.
    pub fn parse(status: &str, missing: &str, requests: &str) -> Option<Self> {
        match status {
            "ready" => Some(Self::Ready),
            "restart" => Some(Self::Restart),
            "degraded" => {
                let named = named_requests(requests);
                Some(Self::Degraded(if named.is_empty() {
                    missing
                        .split(',')
                        .map(str::trim)
                        .filter(|task| !task.is_empty())
                        .map(str::to_owned)
                        .collect()
                } else {
                    named
                }))
            }
            _ => None,
        }
    }

    /// The missing tasks as the user asked for them. The daemon names the
    /// missing leg of a translation it pivots through English (`fa>en` for
    /// `fa>fr`), but the user chose `fa>fr`: each leg becomes the pair in
    /// `asked` (the active chain's translations, from and to) it serves.
    pub fn as_asked(self, asked: &[(String, String)]) -> Self {
        let Self::Degraded(tasks) = self else {
            return self;
        };
        let mut named: Vec<String> = Vec::new();
        for task in tasks {
            let task = pair_of(&task)
                .and_then(|(from, to)| {
                    asked.iter().find(|(a, b)| match (from, to) {
                        (_, "en") => a == from && b != "en",
                        ("en", _) => b == to && a != "en",
                        _ => false,
                    })
                })
                .map_or(task, |(a, b)| format!("text.translate:{a}>{b}"));
            if !named.contains(&task) {
                named.push(task);
            }
        }
        Self::Degraded(named)
    }

    /// The banner's sentence, or `None` when there is nothing to advise.
    pub fn notice(&self) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::Restart => Some(gettextrs::gettext(
                "A model was connected after Myna started. Restart dictation to use it.",
            )),
            Self::Degraded(tasks) => {
                // TRANSLATORS: {models} names the kinds of model that are missing, such as "translation" or "text cleanup and translation".
                let frame = gettextrs::gettext(
                    "This needs a model for {models}, which is not installed. Myna types what it heard instead.",
                );
                Some(frame.replace("{models}", &names(tasks)))
            }
        }
    }

    /// One line for the Diagnostics report.
    pub fn summary(&self) -> String {
        match self {
            Self::Ready => gettextrs::gettext("ready"),
            Self::Restart => gettextrs::gettext("restart dictation to use a new model"),
            Self::Degraded(tasks) => {
                // TRANSLATORS: {models} names the kinds of model that are missing, such as "translation".
                let frame = gettextrs::gettext("missing a model for {models}");
                frame.replace("{models}", &names(tasks))
            }
        }
    }
}

/// The tasks of the daemon's `ChainMissingRequests`, a translation with its
/// languages as `text.translate:en>fr` (`text.translate>fr` when the source
/// is not named); empty for a daemon too old to say.
fn named_requests(json: &str) -> Vec<String> {
    let Ok(serde_json::Value::Array(requests)) = serde_json::from_str(json) else {
        return Vec::new();
    };
    requests
        .iter()
        .filter_map(|request| {
            let task = request.get("task")?.as_str()?;
            let params = request.get("params")?;
            let text = |name| params.get(name).and_then(|v| v.as_str());
            Some(match (text("language"), text("to")) {
                (Some(from), Some(to)) if task == "text.translate" => {
                    format!("{task}:{from}>{to}")
                }
                (_, Some(to)) if task == "text.translate" => format!("{task}>{to}"),
                _ => task.to_owned(),
            })
        })
        .collect()
}

/// The languages of a `text.translate:en>fr` task.
fn pair_of(task: &str) -> Option<(&str, &str)> {
    task.strip_prefix("text.translate:")?.split_once('>')
}

/// A task as the user would call it.
pub fn word(task: &str) -> String {
    if let Some((from, to)) = pair_of(task) {
        // TRANSLATORS: {from} and {to} are language names, such as "English" and "Deutsch".
        let frame = gettextrs::gettext("translation from {from} to {to}");
        return frame
            .replace("{from}", &crate::processing_steps::language_name(from))
            .replace("{to}", &crate::processing_steps::language_name(to));
    }
    if let Some(to) = task.strip_prefix("text.translate>") {
        // TRANSLATORS: {language} is a language name, such as "Français".
        let frame = gettextrs::gettext("translation to {language}");
        return frame.replace("{language}", &crate::processing_steps::language_name(to));
    }
    match task {
        "text.cleanup" => gettextrs::gettext("text cleanup"),
        "text.translate" => gettextrs::gettext("translation"),
        "text.rewrite" => gettextrs::gettext("rewriting"),
        _ => gettextrs::gettext("text"),
    }
}

fn names(tasks: &[String]) -> String {
    let mut words: Vec<String> = tasks.iter().map(|task| word(task)).collect();
    words.dedup();
    if words.is_empty() {
        words.push(word(""));
    }
    // TRANSLATORS: separates the kinds of model in a list, as in "text cleanup, translation".
    words.join(&gettextrs::gettext(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_daemon_or_an_unknown_value_says_nothing() {
        assert_eq!(ChainState::parse("", "", ""), None);
        assert_eq!(ChainState::parse("sideways", "text.translate", ""), None);
    }

    #[test]
    fn ready_has_no_notice() {
        let state = ChainState::parse("ready", "", "").expect("ready");
        assert_eq!(state, ChainState::Ready);
        assert_eq!(state.notice(), None);
        assert_eq!(state.summary(), "ready");
    }

    #[test]
    fn a_missing_task_is_named_in_plain_words() {
        let state = ChainState::parse("degraded", "text.translate", "").expect("degraded");
        assert_eq!(
            state.notice().as_deref(),
            Some("This needs a model for translation, which is not installed. Myna types what it heard instead.")
        );
        assert_eq!(state.summary(), "missing a model for translation");
    }

    #[test]
    fn several_tasks_are_listed_once_each() {
        let state = ChainState::parse(
            "degraded",
            "text.cleanup, text.translate,text.translate",
            "",
        )
        .unwrap();
        assert_eq!(
            state,
            ChainState::Degraded(vec![
                "text.cleanup".into(),
                "text.translate".into(),
                "text.translate".into()
            ])
        );
        assert_eq!(
            state.summary(),
            "missing a model for text cleanup, translation"
        );
    }

    #[test]
    fn an_unknown_or_empty_task_reads_as_text() {
        let unknown = ChainState::parse("degraded", "audio.denoise", "").unwrap();
        assert_eq!(unknown.summary(), "missing a model for text");
        let empty = ChainState::parse("degraded", "", "").unwrap();
        assert_eq!(empty.summary(), "missing a model for text");
    }

    #[test]
    fn the_daemons_full_request_names_the_target() {
        let json = r#"[{"task":"text.translate","params":{"language":"en","to":"fr"}},{"task":"text.cleanup","params":{}}]"#;
        let state = ChainState::parse("degraded", "text.translate", json).unwrap();
        assert_eq!(
            state.notice().as_deref(),
            Some("This needs a model for translation from English to Français, text cleanup, which is not installed. Myna types what it heard instead.")
        );
        let broken = ChainState::parse("degraded", "text.cleanup", "not json").unwrap();
        assert_eq!(broken.summary(), "missing a model for text cleanup");
    }

    fn asked(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| ((*a).into(), (*b).into()))
            .collect()
    }

    #[test]
    fn a_missing_pivot_leg_names_the_pair_the_user_asked_for() {
        let chain = asked(&[("fa", "fr")]);
        for leg in [
            r#"[{"task":"text.translate","params":{"language":"fa","to":"en"}}]"#,
            r#"[{"task":"text.translate","params":{"language":"en","to":"fr"}}]"#,
            r#"[{"task":"text.translate","params":{"language":"fa","to":"en"}},{"task":"text.translate","params":{"language":"en","to":"fr"}}]"#,
        ] {
            let state = ChainState::parse("degraded", "text.translate", leg)
                .unwrap()
                .as_asked(&chain);
            assert_eq!(
                state,
                ChainState::Degraded(vec!["text.translate:fa>fr".into()]),
                "{leg}"
            );
        }
    }

    #[test]
    fn a_pair_the_user_asked_for_is_left_as_it_is() {
        let json = r#"[{"task":"text.translate","params":{"language":"en","to":"fr"}},{"task":"text.translate","params":{"language":"fr","to":"en"}},{"task":"text.cleanup","params":{}}]"#;
        let state = ChainState::parse("degraded", "text.translate", json)
            .unwrap()
            .as_asked(&asked(&[("en", "fr"), ("fr", "en")]));
        assert_eq!(
            state,
            ChainState::Degraded(vec![
                "text.translate:en>fr".into(),
                "text.translate:fr>en".into(),
                "text.cleanup".into()
            ])
        );
        let other = ChainState::parse("degraded", "text.translate", json)
            .unwrap()
            .as_asked(&asked(&[("de", "fa")]));
        assert_eq!(other, ChainState::parse("degraded", "", json).unwrap());
        assert_eq!(
            ChainState::Restart.as_asked(&asked(&[("fa", "fr")])),
            ChainState::Restart
        );
    }

    #[test]
    fn a_request_naming_only_its_target_says_so() {
        let json = r#"[{"task":"text.translate","params":{"to":"de"}}]"#;
        let state = ChainState::parse("degraded", "text.translate", json).unwrap();
        assert_eq!(
            state.summary(),
            "missing a model for translation to Deutsch"
        );
    }

    #[test]
    fn only_a_translation_is_named_by_its_languages() {
        let json = r#"[{"task":"text.cleanup","params":{"language":"en","to":"fr"}},{"task":"text.cleanup","params":{"to":"fr"}}]"#;
        let state = ChainState::parse("degraded", "text.cleanup", json).unwrap();
        assert_eq!(state.summary(), "missing a model for text cleanup");
    }

    #[test]
    fn restart_asks_for_a_restart() {
        let state = ChainState::parse("restart", "text.translate", "").expect("restart");
        assert_eq!(
            state.notice().as_deref(),
            Some("A model was connected after Myna started. Restart dictation to use it.")
        );
        assert_eq!(state.summary(), "restart dictation to use a new model");
    }
}
