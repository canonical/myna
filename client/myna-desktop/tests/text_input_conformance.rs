//! The text input conformance suite against `MockInjector`.
//!
//! The mock cannot show a field, so every field-level assertion is reported
//! unobserved; the lists below pin those gaps so a change to them is seen.

use async_trait::async_trait;
use myna_desktop::inject::mock::{AcquireOutcome, FocusSender, MockInjector};
use myna_desktop::inject::Injector;
use myna_platform::conformance::text_input::{run, Field, FieldKind, FieldView, Fixture};

struct MockFixture {
    preedit: bool,
}

struct MockField(FocusSender);

#[async_trait]
impl Field for MockField {
    async fn lose_focus(&mut self) {
        self.0.send(myna_desktop::FocusEvent::FocusOut);
    }

    // Every acquire mints a fresh lease, which starts focused.
    async fn focus(&mut self) {}

    async fn observe(&mut self) -> Option<FieldView> {
        None
    }
}

#[async_trait]
impl Fixture for MockFixture {
    async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn Field>) {
        let mut injector = MockInjector::new();
        if kind == FieldKind::Secure {
            injector = injector.with_acquires([AcquireOutcome::Secure]);
        }
        if self.preedit {
            injector = injector.with_preedit_support();
        }
        let field = MockField(injector.focus_sender());
        (Box::new(injector), Box::new(field))
    }
}

/// The checks with nothing to observe in the field.
const CONTRACT_ONLY: [&str; 3] = [
    "late_focus_streams_still_report_the_loss",
    "a_focused_target_reports_nothing",
    "release_after_focus_loss_then_reacquire",
];

#[tokio::test]
async fn a_commit_only_mock_conforms() {
    let report = run(&mut MockFixture { preedit: false }).await;
    assert_eq!(report.passed, CONTRACT_ONLY);
    assert_eq!(
        report.unobserved,
        [
            "commits_reach_a_plain_field",
            "no_commit_after_focus_loss",
            "a_newer_target_supersedes_the_older",
            "secure_fields_are_refused",
            "preedit_without_support_is_inert",
        ]
    );
    assert_eq!(
        report.not_applicable,
        ["commit_clears_the_preedit", "no_preedit_after_focus_loss"]
    );
}

#[tokio::test]
async fn a_preedit_mock_conforms() {
    let report = run(&mut MockFixture { preedit: true }).await;
    assert_eq!(report.passed, CONTRACT_ONLY);
    assert_eq!(
        report.unobserved,
        [
            "commits_reach_a_plain_field",
            "no_commit_after_focus_loss",
            "a_newer_target_supersedes_the_older",
            "secure_fields_are_refused",
            "commit_clears_the_preedit",
            "no_preedit_after_focus_loss",
        ]
    );
    assert_eq!(report.not_applicable, ["preedit_without_support_is_inert"]);
}
