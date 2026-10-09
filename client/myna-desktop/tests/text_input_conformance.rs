//! The text input conformance suite against `MockInjector`, whose field the
//! fixture can see, so every applicable check runs in full.

use async_trait::async_trait;
use myna_desktop::inject::mock::{MockField, MockInjector};
use myna_desktop::inject::Injector;
use myna_desktop::FocusEvent;
use myna_platform::conformance::text_input::{run, Field, FieldKind, FieldView, Fixture};

struct MockFixture {
    preedit: bool,
}

struct SeenField(MockField);

#[async_trait]
impl Field for SeenField {
    async fn lose_focus(&mut self) {
        self.0.lose_focus(FocusEvent::FocusOut);
    }

    // Every acquire binds the field afresh, as if refocused.
    async fn focus(&mut self) {}

    async fn blip(&mut self) {
        self.0.blip();
    }

    // The mock has no clock: a held blip is a blip.
    async fn grab(&mut self) {}

    async fn ungrab(&mut self) {
        self.0.blip();
    }

    async fn turn_secure(&mut self) {
        self.0.set_secure(true);
    }

    async fn observe(&mut self) -> Option<FieldView> {
        Some(FieldView {
            text: self.0.text(),
            preedit: self.0.preedit(),
        })
    }
}

#[async_trait]
impl Fixture for MockFixture {
    async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn Field>) {
        let mut injector = MockInjector::new();
        if self.preedit {
            injector = injector.with_preedit_support();
        }
        let field = injector.field();
        field.set_secure(kind == FieldKind::Secure);
        (Box::new(injector), Box::new(SeenField(field)))
    }
}

const COMMON: [&str; 11] = [
    "commits_reach_a_plain_field",
    "no_commit_after_focus_loss",
    "late_focus_streams_still_report_the_loss",
    "a_focused_target_reports_nothing",
    "a_newer_target_supersedes_the_older",
    "release_after_focus_loss_then_reacquire",
    "a_focus_blip_with_an_activation_is_not_a_loss",
    "a_focus_blip_without_an_activation_is_a_loss",
    "a_blip_kept_alive_by_continued_activation_is_not_a_loss",
    "secure_fields_are_refused",
    "a_field_turning_secure_is_refused",
];

#[tokio::test(start_paused = true)]
async fn a_commit_only_mock_conforms() {
    let report = run(&mut MockFixture { preedit: false }).await;
    let mut passed = COMMON.to_vec();
    passed.push("preedit_without_support_is_inert");
    assert_eq!(report.passed, passed);
    assert!(report.unobserved.is_empty(), "{report:?}");
    assert_eq!(
        report.not_applicable,
        [
            "commit_clears_the_preedit",
            "no_preedit_after_focus_loss",
            "release_clears_the_preedit",
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn a_preedit_mock_conforms() {
    let report = run(&mut MockFixture { preedit: true }).await;
    let mut passed = COMMON.to_vec();
    passed.extend([
        "commit_clears_the_preedit",
        "no_preedit_after_focus_loss",
        "release_clears_the_preedit",
    ]);
    assert_eq!(report.passed, passed);
    assert!(report.unobserved.is_empty(), "{report:?}");
    assert_eq!(report.not_applicable, ["preedit_without_support_is_inert"]);
}
