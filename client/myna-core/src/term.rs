//! Terminal-output-mode decisions (feature 011-accessible-dictation-ux, US5,
//! FR-028/029) shared by every terminal-facing surface so a single
//! convention decides "should this avoid in-place redraw/animation", rather
//! than each call site guessing independently.

/// Pure decision: should terminal output avoid in-place redraw/animation
/// (FR-029) and rely only on explicit textual markers, never colour/emoji
/// alone (FR-028)? Parameterized by the raw `NO_COLOR` value so it's
/// directly hermetically testable without mutating the real process
/// environment (`plain_output` below is the thin env-reading wrapper).
///
/// Follows the [NO_COLOR](https://no-color.org/) convention referenced
/// directly by `specs/011-accessible-dictation-ux/quickstart.md`'s Scenario
/// 8 (`NO_COLOR=1`): the variable being *set at all* means "avoid
/// decoration", regardless of its value (per the NO_COLOR spec).
pub fn plain_output_requested(no_color: Option<&str>) -> bool {
    no_color.is_some()
}

/// [`plain_output_requested`] against the real `NO_COLOR` environment
/// variable.
pub fn plain_output() -> bool {
    plain_output_requested(std::env::var("NO_COLOR").ok().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_is_not_plain_output() {
        assert!(!plain_output_requested(None));
    }

    #[test]
    fn set_to_any_value_requests_plain_output() {
        // The NO_COLOR convention: presence, not content, is the signal.
        assert!(plain_output_requested(Some("1")));
        assert!(plain_output_requested(Some("")));
        assert!(plain_output_requested(Some("0")));
        assert!(plain_output_requested(Some("false")));
    }
}
