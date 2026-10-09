//! The text input suite: the backend-neutral invariants of
//! [`crate::text_input`].

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;

use super::Report;
use crate::text_input::{FocusEvent, InjectError, Injector, Support, Target};

/// How long a backend may take to report a focus loss.
const FOCUS_EVENT_LIMIT: Duration = Duration::from_secs(5);
/// How long a focused target must stay quiet.
const QUIET: Duration = Duration::from_millis(100);
/// How often a write is retried while a change to the field reaches the
/// backend.
const RETRY: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Plain,
    /// A password field, as the desktop marks one.
    Secure,
}

/// What a field shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldView {
    pub text: String,
    pub preedit: String,
}

/// The test's hand on the field an injector writes into.
#[async_trait]
pub trait Field: Send {
    /// Move focus off the field, as the user would.
    async fn lose_focus(&mut self);

    /// Give the field focus again.
    async fn focus(&mut self);

    /// Move focus off the field and straight back to it, as an X11 key grab
    /// does for as long as a shortcut is held.
    async fn blip(&mut self);

    /// Make the focused field secure under a held target, as a page that
    /// swaps a text input for a password one.
    async fn turn_secure(&mut self);

    /// What the field shows now; `None` where this fixture cannot see it.
    async fn observe(&mut self) -> Option<FieldView>;
}

/// Sets up one case: a fresh injector and a focused field of `kind`.
#[async_trait]
pub trait Fixture: Send {
    async fn setup(&mut self, kind: FieldKind) -> (Box<dyn Injector>, Box<dyn Field>);
}

/// Run every check against `fixture`'s backend. Panics on a violation.
pub async fn run(fixture: &mut dyn Fixture) -> Report {
    let mut report = Report::default();
    let (injector, _) = fixture.setup(FieldKind::Plain).await;
    let capabilities = injector.capabilities();
    drop(injector);

    let observed = commits_reach_a_plain_field(fixture).await;
    report.record("commits_reach_a_plain_field", observed);
    let observed = no_commit_after_focus_loss(fixture).await;
    report.record("no_commit_after_focus_loss", observed);
    late_focus_streams_still_report_the_loss(fixture).await;
    report.record("late_focus_streams_still_report_the_loss", true);
    a_focused_target_reports_nothing(fixture).await;
    report.record("a_focused_target_reports_nothing", true);
    let observed = a_newer_target_supersedes_the_older(fixture).await;
    report.record("a_newer_target_supersedes_the_older", observed);
    release_after_focus_loss_then_reacquire(fixture).await;
    report.record("release_after_focus_loss_then_reacquire", true);
    let observed = a_focus_blip_with_an_activation_is_not_a_loss(fixture).await;
    report.record("a_focus_blip_with_an_activation_is_not_a_loss", observed);
    let observed = a_focus_blip_without_an_activation_is_a_loss(fixture).await;
    report.record("a_focus_blip_without_an_activation_is_a_loss", observed);

    if capabilities.secure_field_detection == Support::Supported {
        let observed = secure_fields_are_refused(fixture).await;
        report.record("secure_fields_are_refused", observed);
        let observed = a_field_turning_secure_is_refused(fixture).await;
        report.record("a_field_turning_secure_is_refused", observed);
    } else {
        report.not_applicable.push("secure_fields_are_refused");
        report
            .not_applicable
            .push("a_field_turning_secure_is_refused");
    }

    if capabilities.preedit {
        let observed = commit_clears_the_preedit(fixture).await;
        report.record("commit_clears_the_preedit", observed);
        let observed = no_preedit_after_focus_loss(fixture).await;
        report.record("no_preedit_after_focus_loss", observed);
        let observed = release_clears_the_preedit(fixture).await;
        report.record("release_clears_the_preedit", observed);
        report
            .not_applicable
            .push("preedit_without_support_is_inert");
    } else {
        report.not_applicable.push("commit_clears_the_preedit");
        report.not_applicable.push("no_preedit_after_focus_loss");
        report.not_applicable.push("release_clears_the_preedit");
        let observed = preedit_without_support_is_inert(fixture).await;
        report.record("preedit_without_support_is_inert", observed);
    }
    report
}

async fn acquire(injector: &mut dyn Injector, check: &str) -> Box<dyn Target> {
    match injector.acquire().await {
        Ok(target) => target,
        Err(err) => panic!("{check}: acquiring a plain focused field failed: {err}"),
    }
}

async fn expect_loss(target: &dyn Target, check: &str) {
    let mut events = target.focus_events();
    match tokio::time::timeout(FOCUS_EVENT_LIMIT, events.next()).await {
        Ok(Some(FocusEvent::FocusOut | FocusEvent::TargetGone)) => {}
        Ok(None) => panic!("{check}: the focus stream ended without reporting the loss"),
        Err(_) => panic!("{check}: no focus event within {FOCUS_EVENT_LIMIT:?}"),
    }
}

async fn commits_reach_a_plain_field(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "commits_reach_a_plain_field";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    if let Err(err) = target.commit("hello").await {
        panic!("{CHECK}: commit into a held field failed: {err}");
    }
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(view.text.contains("hello"), "{CHECK}: field shows {view:?}");
    }
    target.release().await;
    view.is_some()
}

async fn no_commit_after_focus_loss(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "no_commit_after_focus_loss";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    let mut events = target.focus_events();
    field.lose_focus().await;
    match tokio::time::timeout(FOCUS_EVENT_LIMIT, events.next()).await {
        Ok(Some(_)) => {}
        Ok(None) => panic!("{CHECK}: the live focus stream ended without an event"),
        Err(_) => panic!("{CHECK}: no focus event within {FOCUS_EVENT_LIMIT:?}"),
    }
    let refused = target.commit("stray").await;
    assert!(
        matches!(refused, Err(InjectError::FocusLost)),
        "{CHECK}: commit after focus loss gave {refused:?}"
    );
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            !view.text.contains("stray"),
            "{CHECK}: field shows {view:?}"
        );
    }
    target.release().await;
    view.is_some()
}

async fn late_focus_streams_still_report_the_loss(fixture: &mut dyn Fixture) {
    const CHECK: &str = "late_focus_streams_still_report_the_loss";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let target = acquire(injector.as_mut(), CHECK).await;
    field.lose_focus().await;
    expect_loss(target.as_ref(), CHECK).await;
    target.release().await;
}

async fn a_focused_target_reports_nothing(fixture: &mut dyn Fixture) {
    const CHECK: &str = "a_focused_target_reports_nothing";
    let (mut injector, _field) = fixture.setup(FieldKind::Plain).await;
    let target = acquire(injector.as_mut(), CHECK).await;
    let mut events = target.focus_events();
    let early = tokio::time::timeout(QUIET, events.next()).await;
    assert!(early.is_err(), "{CHECK}: reported {early:?} while focused");
    target.release().await;
}

async fn a_newer_target_supersedes_the_older(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "a_newer_target_supersedes_the_older";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut older = acquire(injector.as_mut(), CHECK).await;
    let mut newer = acquire(injector.as_mut(), CHECK).await;
    let refused = older.commit("old").await;
    assert!(
        matches!(refused, Err(InjectError::FocusLost)),
        "{CHECK}: the superseded target wrote: {refused:?}"
    );
    expect_loss(older.as_ref(), CHECK).await;
    older.release().await;
    if let Err(err) = newer.commit("new").await {
        panic!("{CHECK}: releasing the older target broke the newer: {err}");
    }
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            view.text.contains("new") && !view.text.contains("old"),
            "{CHECK}: field shows {view:?}"
        );
    }
    newer.release().await;
    view.is_some()
}

async fn release_after_focus_loss_then_reacquire(fixture: &mut dyn Fixture) {
    const CHECK: &str = "release_after_focus_loss_then_reacquire";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let target = acquire(injector.as_mut(), CHECK).await;
    field.lose_focus().await;
    expect_loss(target.as_ref(), CHECK).await;
    target.release().await;
    field.focus().await;
    let mut again = acquire(injector.as_mut(), CHECK).await;
    if let Err(err) = again.commit("again").await {
        panic!("{CHECK}: a reacquired target cannot write: {err}");
    }
    again.release().await;
}

async fn a_focus_blip_with_an_activation_is_not_a_loss(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "a_focus_blip_with_an_activation_is_not_a_loss";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    let mut events = target.focus_events();
    target.activated();
    field.blip().await;
    if let Err(err) = target.commit("after").await {
        panic!("{CHECK}: commit after the blip failed: {err}");
    }
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(view.text.contains("after"), "{CHECK}: field shows {view:?}");
    }
    let early = tokio::time::timeout(QUIET, events.next()).await;
    assert!(early.is_err(), "{CHECK}: reported {early:?} for a blip");
    target.release().await;
    view.is_some()
}

/// Without an activation a blip may be a move to another field that shares
/// the context, as an application with one context per window makes it.
async fn a_focus_blip_without_an_activation_is_a_loss(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "a_focus_blip_without_an_activation_is_a_loss";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    let events = target.focus_events();
    field.blip().await;
    let stray = target.commit("stray").await;
    let mut events = events;
    match tokio::time::timeout(FOCUS_EVENT_LIMIT, events.next()).await {
        Ok(Some(_)) => {}
        Ok(None) => panic!("{CHECK}: the focus stream ended without an event"),
        Err(_) => panic!("{CHECK}: no focus event within {FOCUS_EVENT_LIMIT:?}"),
    }
    assert!(
        matches!(stray, Err(InjectError::FocusLost)),
        "{CHECK}: commit after the blip gave {stray:?}"
    );
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            !view.text.contains("stray"),
            "{CHECK}: field shows {view:?}"
        );
    }
    target.release().await;
    view.is_some()
}

async fn secure_fields_are_refused(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "secure_fields_are_refused";
    let (mut injector, mut field) = fixture.setup(FieldKind::Secure).await;
    match injector.acquire().await {
        Err(InjectError::SecureField) => {}
        Ok(mut target) => {
            // Content type may arrive after acquire; the write must still fail.
            let refused = target.commit("secret").await;
            assert!(
                matches!(refused, Err(InjectError::SecureField)),
                "{CHECK}: commit into a secure field gave {refused:?}"
            );
            target.set_preedit("secret").await;
            target.release().await;
        }
        Err(err) => panic!("{CHECK}: acquire failed for another reason: {err}"),
    }
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            !view.text.contains("secret") && !view.preedit.contains("secret"),
            "{CHECK}: field shows {view:?}"
        );
    }
    view.is_some()
}

async fn a_field_turning_secure_is_refused(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "a_field_turning_secure_is_refused";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    field.turn_secure().await;
    // The desktop may tell the backend after the field changed: probes may
    // land until it knows, and must stop within the limit.
    let deadline = tokio::time::Instant::now() + FOCUS_EVENT_LIMIT;
    loop {
        match target.commit("probe").await {
            Err(InjectError::SecureField) => break,
            Ok(()) if tokio::time::Instant::now() < deadline => tokio::time::sleep(RETRY).await,
            Ok(()) => panic!("{CHECK}: commits still land {FOCUS_EVENT_LIMIT:?} after it turned"),
            Err(err) => panic!("{CHECK}: commit gave {err}, not a secure field refusal"),
        }
    }
    target.set_preedit("secret").await;
    // Before the commit, which may clear a preedit it should never have shown.
    if let Some(view) = field.observe().await {
        assert!(
            !view.preedit.contains("secret"),
            "{CHECK}: field shows {view:?}"
        );
    }
    let refused = target.commit("secret").await;
    assert!(
        matches!(refused, Err(InjectError::SecureField)),
        "{CHECK}: commit once refused gave {refused:?}"
    );
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            !view.text.contains("secret"),
            "{CHECK}: field shows {view:?}"
        );
    }
    target.release().await;
    view.is_some()
}

async fn release_clears_the_preedit(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "release_clears_the_preedit";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    target.set_preedit("draft").await;
    let before = field.observe().await;
    if let Some(view) = &before {
        assert_eq!(view.preedit, "draft", "{CHECK}: field shows {view:?}");
    }
    target.release().await;
    let after = field.observe().await;
    if let Some(view) = &after {
        assert!(view.preedit.is_empty(), "{CHECK}: field shows {view:?}");
    }
    after.is_some()
}

async fn commit_clears_the_preedit(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "commit_clears_the_preedit";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    target.set_preedit("draft").await;
    let before = field.observe().await;
    if let Some(view) = &before {
        assert_eq!(view.preedit, "draft", "{CHECK}: field shows {view:?}");
    }
    if let Err(err) = target.commit("final").await {
        panic!("{CHECK}: commit failed: {err}");
    }
    let after = field.observe().await;
    if let Some(view) = &after {
        assert!(
            view.preedit.is_empty() && view.text.contains("final") && !view.text.contains("draft"),
            "{CHECK}: field shows {view:?}"
        );
    }
    target.release().await;
    after.is_some()
}

async fn no_preedit_after_focus_loss(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "no_preedit_after_focus_loss";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    field.lose_focus().await;
    expect_loss(target.as_ref(), CHECK).await;
    target.set_preedit("stray").await;
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(
            !view.preedit.contains("stray"),
            "{CHECK}: field shows {view:?}"
        );
    }
    target.release().await;
    view.is_some()
}

async fn preedit_without_support_is_inert(fixture: &mut dyn Fixture) -> bool {
    const CHECK: &str = "preedit_without_support_is_inert";
    let (mut injector, mut field) = fixture.setup(FieldKind::Plain).await;
    let mut target = acquire(injector.as_mut(), CHECK).await;
    target.set_preedit("draft").await;
    let view = field.observe().await;
    if let Some(view) = &view {
        assert!(view.preedit.is_empty(), "{CHECK}: field shows {view:?}");
    }
    target.release().await;
    view.is_some()
}
