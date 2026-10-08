//! Conformance suites every backend of a module must pass, mocks included.
//!
//! A suite drives a backend through a fixture its test supplies, panics on
//! the first violation, and returns a [`Report`] naming what it could not
//! check, so a test asserts the gaps it expects instead of skipping silently.

pub mod text_input;

/// What a suite run established.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Checks that ran in full.
    pub passed: Vec<&'static str>,
    /// Checks whose contract-level assertions passed but whose field-level
    /// ones were skipped: the fixture cannot see the field.
    pub unobserved: Vec<&'static str>,
    /// Checks that do not apply to the backend's capabilities.
    pub not_applicable: Vec<&'static str>,
}

impl Report {
    fn record(&mut self, check: &'static str, observed: bool) {
        if observed {
            self.passed.push(check);
        } else {
            self.unobserved.push(check);
        }
    }
}
