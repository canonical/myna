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
    pub fn parse(status: &str, missing: &str) -> Option<Self> {
        match status {
            "ready" => Some(Self::Ready),
            "restart" => Some(Self::Restart),
            "degraded" => Some(Self::Degraded(
                missing
                    .split(',')
                    .map(str::trim)
                    .filter(|task| !task.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )),
            _ => None,
        }
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
                    "This needs a {models} model that is not installed. Myna types what it heard instead.",
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
                let frame = gettextrs::gettext("missing a {models} model");
                frame.replace("{models}", &names(tasks))
            }
        }
    }
}

/// A task as the user would call it.
fn word(task: &str) -> String {
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
        assert_eq!(ChainState::parse("", ""), None);
        assert_eq!(ChainState::parse("sideways", "text.translate"), None);
    }

    #[test]
    fn ready_has_no_notice() {
        let state = ChainState::parse("ready", "").expect("ready");
        assert_eq!(state, ChainState::Ready);
        assert_eq!(state.notice(), None);
        assert_eq!(state.summary(), "ready");
    }

    #[test]
    fn a_missing_task_is_named_in_plain_words() {
        let state = ChainState::parse("degraded", "text.translate").expect("degraded");
        assert_eq!(
            state.notice().as_deref(),
            Some("This needs a translation model that is not installed. Myna types what it heard instead.")
        );
        assert_eq!(state.summary(), "missing a translation model");
    }

    #[test]
    fn several_tasks_are_listed_once_each() {
        let state =
            ChainState::parse("degraded", "text.cleanup, text.translate,text.translate").unwrap();
        assert_eq!(
            state,
            ChainState::Degraded(vec![
                "text.cleanup".into(),
                "text.translate".into(),
                "text.translate".into()
            ])
        );
        assert_eq!(state.summary(), "missing a text cleanup, translation model");
    }

    #[test]
    fn an_unknown_or_empty_task_reads_as_text() {
        let unknown = ChainState::parse("degraded", "audio.denoise").unwrap();
        assert_eq!(unknown.summary(), "missing a text model");
        let empty = ChainState::parse("degraded", "").unwrap();
        assert_eq!(empty.summary(), "missing a text model");
    }

    #[test]
    fn restart_asks_for_a_restart() {
        let state = ChainState::parse("restart", "text.translate").expect("restart");
        assert_eq!(
            state.notice().as_deref(),
            Some("A model was connected after Myna started. Restart dictation to use it.")
        );
        assert_eq!(state.summary(), "restart dictation to use a new model");
    }
}
