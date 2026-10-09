//! The text input suite against an in-memory reference backend whose field
//! can be seen, and against that backend with one flaw each, which the suite
//! must reject.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use tokio::sync::watch;
use tokio::time::Instant;

use myna_platform::conformance::text_input::{run, Field, FieldKind, FieldView, Fixture};
use myna_platform::conformance::Report;
use myna_platform::text_input::{
    FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flaw {
    None,
    CommitsAfterLoss,
    NeverReportsLoss,
    ReportsWhileFocused,
    SupersededStillWrites,
    WritesSecureFields,
    DropsCommits,
    KeepsPreeditOnCommit,
    PreeditAfterLoss,
    PreeditWithoutSupport,
    BreaksOnRelease,
    LateStreamsMissTheLoss,
    CommitsThePreedit,
    WritesFieldsTurnedSecure,
    PreeditInSecureFields,
    KeepsPreeditOnRelease,
    BlipIsALoss,
    BlipIsNeverALoss,
    GraceIgnoresActivation,
}

/// How long the reference backend rides out a blip after its last
/// activation.
const GRACE: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
struct Desk {
    lease: u64,
    lost: bool,
    /// The held target was told of an activation.
    activated: bool,
    /// When it was last told.
    activated_at: Option<Instant>,
    /// When a grab took focus off the field.
    grabbed_at: Option<Instant>,
    focused: bool,
    secure: bool,
    /// Commits a field turned secure still takes before the backend hears.
    lag: u32,
    blind: u32,
    broken: bool,
    text: String,
    preedit: String,
}

struct Reference {
    desk: Arc<watch::Sender<Desk>>,
    capabilities: TextInputCapabilities,
    flaw: Flaw,
}

#[derive(Debug)]
struct RefTarget {
    id: u64,
    desk: Arc<watch::Sender<Desk>>,
    capabilities: TextInputCapabilities,
    flaw: Flaw,
}

impl RefTarget {
    fn owned(&self) -> bool {
        let desk = self.desk.borrow();
        desk.lease == self.id && !desk.lost
    }

    fn may_write(&self) -> bool {
        self.owned()
            || match self.flaw {
                Flaw::CommitsAfterLoss => !self.desk.borrow().focused,
                Flaw::SupersededStillWrites => self.desk.borrow().lease != self.id,
                _ => false,
            }
    }
}

#[async_trait]
impl Injector for Reference {
    async fn acquire(&mut self) -> Result<Box<dyn Target>, InjectError> {
        let mut id = 0;
        self.desk.send_modify(|desk| {
            desk.lease += 1;
            desk.lost = !desk.focused;
            desk.activated = false;
            id = desk.lease;
        });
        let desk = self.desk.borrow();
        if desk.broken {
            return Err(InjectError::Unavailable("broken by a release".into()));
        }
        if !desk.focused {
            return Err(InjectError::NoTarget);
        }
        let detects = self.capabilities.secure_field_detection == Support::Supported;
        if desk.secure && detects && self.flaw != Flaw::WritesSecureFields {
            return Err(InjectError::SecureField);
        }
        Ok(Box::new(RefTarget {
            id,
            desk: Arc::clone(&self.desk),
            capabilities: self.capabilities,
            flaw: self.flaw,
        }))
    }

    fn capabilities(&self) -> TextInputCapabilities {
        self.capabilities
    }
}

#[async_trait]
impl Target for RefTarget {
    async fn commit(&mut self, text: &str) -> Result<(), InjectError> {
        if !self.may_write() {
            return Err(InjectError::FocusLost);
        }
        let detects = self.capabilities.secure_field_detection == Support::Supported;
        let ignores = matches!(
            self.flaw,
            Flaw::WritesSecureFields | Flaw::WritesFieldsTurnedSecure
        );
        if detects && !ignores && self.desk.borrow().secure && self.desk.borrow().blind == 0 {
            return Err(InjectError::SecureField);
        }
        let flaw = self.flaw;
        self.desk.send_modify(|desk| {
            desk.blind = desk.blind.saturating_sub(1);
            if flaw == Flaw::CommitsThePreedit {
                let preedit = desk.preedit.clone();
                desk.text.push_str(&preedit);
            }
            if flaw != Flaw::KeepsPreeditOnCommit {
                desk.preedit.clear();
            }
            if flaw != Flaw::DropsCommits {
                desk.text.push_str(text);
            }
        });
        Ok(())
    }

    async fn set_preedit(&mut self, text: &str) {
        let supported = self.capabilities.preedit || self.flaw == Flaw::PreeditWithoutSupport;
        let allowed = self.owned() || self.flaw == Flaw::PreeditAfterLoss;
        let shown = !self.desk.borrow().secure || self.flaw == Flaw::PreeditInSecureFields;
        if supported && allowed && shown {
            self.desk.send_modify(|desk| desk.preedit = text.to_owned());
        }
    }

    fn activated(&self) {
        let id = self.id;
        self.desk.send_modify(|desk| {
            if desk.lease == id {
                desk.activated = true;
                desk.activated_at = Some(Instant::now());
            }
        });
    }

    fn focus_events(&self) -> BoxStream<'static, FocusEvent> {
        let id = self.id;
        match self.flaw {
            Flaw::NeverReportsLoss => stream::pending().boxed(),
            Flaw::ReportsWhileFocused => stream::once(async { FocusEvent::FocusOut }).boxed(),
            Flaw::LateStreamsMissTheLoss if !self.owned() => stream::pending().boxed(),
            _ => {
                let mut desk = self.desk.subscribe();
                stream::once(async move {
                    let _ = desk.wait_for(|d| d.lease != id || d.lost).await;
                    FocusEvent::FocusOut
                })
                .boxed()
            }
        }
    }

    async fn release(self: Box<Self>) {
        let flaw = self.flaw;
        let owned = self.owned();
        self.desk.send_modify(|desk| {
            if owned && flaw != Flaw::KeepsPreeditOnRelease {
                desk.preedit.clear();
            }
            if owned {
                desk.lost = true;
            }
            if flaw == Flaw::BreaksOnRelease {
                desk.broken = true;
            }
        });
    }
}

struct RefField(Arc<watch::Sender<Desk>>, Flaw);

#[async_trait]
impl Field for RefField {
    async fn lose_focus(&mut self) {
        self.0.send_modify(|desk| {
            desk.focused = false;
            desk.lost = true;
        });
    }

    async fn focus(&mut self) {
        self.0.send_modify(|desk| desk.focused = true);
    }

    async fn blip(&mut self) {
        let flaw = self.1;
        self.0.send_modify(|desk| {
            desk.preedit.clear();
            let ridden = match flaw {
                Flaw::BlipIsALoss => false,
                Flaw::BlipIsNeverALoss => true,
                _ => desk.activated,
            };
            desk.lost |= !ridden;
        });
    }

    async fn grab(&mut self) {
        self.0.send_modify(|desk| {
            desk.preedit.clear();
            desk.grabbed_at = Some(Instant::now());
        });
    }

    async fn ungrab(&mut self) {
        let flaw = self.1;
        self.0.send_modify(|desk| {
            let grabbed = desk.grabbed_at.take().expect("a grab first");
            let since = match (flaw, desk.activated_at) {
                (Flaw::GraceIgnoresActivation, _) | (_, None) => grabbed,
                (_, Some(at)) => at.max(grabbed),
            };
            desk.lost |= !(desk.activated && since.elapsed() <= GRACE);
        });
    }

    async fn turn_secure(&mut self) {
        self.0.send_modify(|desk| {
            desk.secure = true;
            desk.blind = desk.lag;
        });
    }

    async fn observe(&mut self) -> Option<FieldView> {
        let desk = self.0.borrow();
        Some(FieldView {
            text: desk.text.clone(),
            preedit: desk.preedit.clone(),
        })
    }
}

struct RefFixture {
    capabilities: TextInputCapabilities,
    flaw: Flaw,
    lag: u32,
}

#[async_trait]
impl Fixture for RefFixture {
    async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn Field>) {
        let desk = Arc::new(watch::Sender::new(Desk {
            focused: true,
            secure: kind == FieldKind::Secure,
            lag: self.lag,
            ..Desk::default()
        }));
        let injector = Reference {
            desk: Arc::clone(&desk),
            capabilities: self.capabilities,
            flaw: self.flaw,
        };
        (Box::new(injector), Box::new(RefField(desk, self.flaw)))
    }
}

const FULL: TextInputCapabilities = TextInputCapabilities {
    preedit: true,
    surrounding_text: false,
    secure_field_detection: Support::Supported,
};

async fn suite(capabilities: TextInputCapabilities, flaw: Flaw) -> Report {
    run(&mut RefFixture {
        capabilities,
        flaw,
        lag: 0,
    })
    .await
}

/// A desktop may tell the backend a field turned secure only after a few
/// writes went in, as IBus does; that is no violation while it stops.
#[tokio::test(start_paused = true)]
async fn a_backend_told_late_of_a_secure_field_still_conforms() {
    let report = run(&mut RefFixture {
        capabilities: FULL,
        flaw: Flaw::None,
        lag: 3,
    })
    .await;
    assert!(report.passed.contains(&"a_field_turning_secure_is_refused"));
}

#[tokio::test(start_paused = true)]
async fn a_backend_with_every_capability_passes_every_check() {
    let report = suite(FULL, Flaw::None).await;
    assert_eq!(
        report.passed,
        [
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
            "commit_clears_the_preedit",
            "no_preedit_after_focus_loss",
            "release_clears_the_preedit",
        ]
    );
    assert!(report.unobserved.is_empty(), "{report:?}");
    assert_eq!(report.not_applicable, ["preedit_without_support_is_inert"]);
}

#[tokio::test(start_paused = true)]
async fn a_commit_only_backend_that_cannot_see_secure_fields_skips_those_checks() {
    let report = suite(TextInputCapabilities::COMMIT_ONLY, Flaw::None).await;
    assert_eq!(report.passed.len(), 10, "{report:?}");
    assert!(report.passed.contains(&"preedit_without_support_is_inert"));
    assert_eq!(
        report.not_applicable,
        [
            "secure_fields_are_refused",
            "a_field_turning_secure_is_refused",
            "commit_clears_the_preedit",
            "no_preedit_after_focus_loss",
            "release_clears_the_preedit",
        ]
    );
}

macro_rules! rejects {
    ($name:ident, $flaw:expr, $capabilities:expr, $check:literal) => {
        #[tokio::test(start_paused = true)]
        #[should_panic(expected = $check)]
        async fn $name() {
            suite($capabilities, $flaw).await;
        }
    };
}

rejects!(
    a_commit_after_focus_loss,
    Flaw::CommitsAfterLoss,
    FULL,
    "no_commit_after_focus_loss"
);
rejects!(
    a_silent_focus_loss,
    Flaw::NeverReportsLoss,
    FULL,
    "no_commit_after_focus_loss"
);
rejects!(
    a_loss_reported_while_focused,
    Flaw::ReportsWhileFocused,
    FULL,
    "a_focused_target_reports_nothing"
);
rejects!(
    a_superseded_target_that_writes,
    Flaw::SupersededStillWrites,
    FULL,
    "a_newer_target_supersedes_the_older"
);
rejects!(
    a_write_into_a_secure_field,
    Flaw::WritesSecureFields,
    FULL,
    "secure_fields_are_refused"
);
rejects!(
    a_commit_that_never_lands,
    Flaw::DropsCommits,
    FULL,
    "commits_reach_a_plain_field"
);
rejects!(
    a_preedit_that_survives_commit,
    Flaw::KeepsPreeditOnCommit,
    FULL,
    "commit_clears_the_preedit"
);
rejects!(
    a_preedit_after_focus_loss,
    Flaw::PreeditAfterLoss,
    FULL,
    "no_preedit_after_focus_loss"
);
rejects!(
    a_preedit_the_backend_does_not_claim,
    Flaw::PreeditWithoutSupport,
    TextInputCapabilities::COMMIT_ONLY,
    "preedit_without_support_is_inert"
);
rejects!(
    a_release_that_breaks_the_next_acquire,
    Flaw::BreaksOnRelease,
    FULL,
    "release_after_focus_loss_then_reacquire"
);
rejects!(
    a_late_stream_that_misses_the_loss,
    Flaw::LateStreamsMissTheLoss,
    FULL,
    "late_focus_streams_still_report_the_loss"
);
rejects!(
    a_commit_into_a_field_turned_secure,
    Flaw::WritesFieldsTurnedSecure,
    FULL,
    "a_field_turning_secure_is_refused"
);
rejects!(
    a_preedit_in_a_field_turned_secure,
    Flaw::PreeditInSecureFields,
    FULL,
    "a_field_turning_secure_is_refused"
);
rejects!(
    a_preedit_left_showing_after_release,
    Flaw::KeepsPreeditOnRelease,
    FULL,
    "release_clears_the_preedit"
);
rejects!(
    a_blip_taken_for_a_loss,
    Flaw::BlipIsALoss,
    FULL,
    "a_focus_blip_with_an_activation_is_not_a_loss"
);
rejects!(
    a_blip_always_ridden_out,
    Flaw::BlipIsNeverALoss,
    FULL,
    "a_focus_blip_without_an_activation_is_a_loss"
);
rejects!(
    a_preedit_committed_with_the_text,
    Flaw::CommitsThePreedit,
    FULL,
    "commit_clears_the_preedit"
);
rejects!(
    a_grace_counted_from_the_grab,
    Flaw::GraceIgnoresActivation,
    FULL,
    "a_blip_kept_alive_by_continued_activation_is_not_a_loss"
);
