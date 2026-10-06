//! Installing a snap from the onboarding rows: one request as the user, then
//! its change followed to the end, its download reported as it grows.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use async_trait::async_trait;
use gtk4::glib::MainContext;
use myna_config::active_backend::SwitchPlan;
use myna_config::backend_apply::ApplyPreview;
use myna_config::command::CancellationToken;
use myna_config::domain::CommandResult;
use myna_config::ports::{
    FailedStep, SystemConfigurator, SystemConfiguratorError, SystemConfiguratorFailure,
};
use myna_config::snap_changes::{parse_change, ChangeInProgress};
use myna_config::snap_install::{follow_change, install, ChangeFailure, Follow};

type Started = Result<Option<String>, SystemConfiguratorError>;

#[derive(Clone, Default)]
struct Snapd {
    started: Rc<RefCell<Option<Started>>>,
    installs: Rc<RefCell<Vec<String>>>,
    reads: Rc<RefCell<VecDeque<Result<ChangeInProgress, String>>>>,
}

impl Snapd {
    fn starting(started: Started) -> Self {
        let snapd = Self::default();
        snapd.started.replace(Some(started));
        snapd
    }

    fn then(self, change: serde_json::Value) -> Self {
        self.reads
            .borrow_mut()
            .push_back(Ok(parse_change(change).unwrap()));
        self
    }

    fn then_unreadable(self, message: &str) -> Self {
        self.reads.borrow_mut().push_back(Err(message.to_owned()));
        self
    }
}

#[async_trait(?Send)]
impl SystemConfigurator for Snapd {
    async fn execute_backend_switch(
        &self,
        _plan: &SwitchPlan,
        _cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        unreachable!("an install switches nothing")
    }

    async fn restart_myna(
        &self,
        _cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        unreachable!("an install restarts nothing")
    }

    async fn apply_backend_config(
        &self,
        _preview: &ApplyPreview,
        _cancellation: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        unreachable!("an install applies nothing")
    }

    async fn user_daemons_enabled(&self, _cancellation: CancellationToken) -> Result<bool, String> {
        unreachable!("an install reads no flag")
    }

    async fn set_up(
        &self,
        _snaps: &[&str],
        _cancellation: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        unreachable!("an install sets nothing up")
    }

    async fn install_snap(
        &self,
        snap: &str,
        _cancellation: CancellationToken,
    ) -> Result<Option<String>, SystemConfiguratorError> {
        self.installs.borrow_mut().push(snap.to_owned());
        self.started.borrow_mut().take().expect("one install")
    }

    async fn snap_change(
        &self,
        change_id: &str,
        _cancellation: CancellationToken,
    ) -> Result<ChangeInProgress, String> {
        assert_eq!(change_id, "12");
        self.reads
            .borrow_mut()
            .pop_front()
            .expect("the change was read after it finished")
    }
}

fn change(ready: bool, downloads: &[(u64, u64)]) -> serde_json::Value {
    let tasks: Vec<serde_json::Value> = downloads
        .iter()
        .map(|(done, total)| {
            let status = if done == total { "Done" } else { "Doing" };
            serde_json::json!({"kind": "download-snap", "status": status,
                "progress": {"label": "myna", "done": done, "total": total}})
        })
        .collect();
    serde_json::json!({"id": "12", "kind": "install-snap", "ready": ready,
        "status": if ready { "Done" } else { "Doing" },
        "summary": "Install \"myna\" snap from \"latest/edge\" channel", "tasks": tasks})
}

fn failed(err: serde_json::Value, status: &str) -> serde_json::Value {
    let mut change = change(true, &[]);
    change["status"] = status.into();
    change["err"] = err;
    change
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    MainContext::new().block_on(future)
}

fn no_sleep(_: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(std::future::ready(()))
}

fn follow(cancellation: CancellationToken) -> Follow<'static> {
    Follow {
        interval: Duration::from_millis(1),
        sleep: &no_sleep,
        cancellation,
    }
}

fn run(snapd: &Snapd) -> (Result<(), SystemConfiguratorError>, Vec<Option<u8>>) {
    let reported = RefCell::new(Vec::new());
    let outcome = block_on(install(
        snapd,
        "myna",
        100,
        &follow(CancellationToken::new()),
        &|percent| reported.borrow_mut().push(percent),
    ));
    (outcome, reported.into_inner())
}

#[test]
fn an_install_reports_its_download_until_the_change_is_done() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned())))
        .then(change(false, &[(1, 1)]))
        .then(change(false, &[(40, 100)]))
        .then(change(false, &[(40, 100)]))
        .then(change(false, &[(90, 100)]))
        .then(change(true, &[(100, 100)]));

    let (outcome, reported) = run(&snapd);

    assert_eq!(outcome, Ok(()));
    assert_eq!(*snapd.installs.borrow(), ["myna"]);
    // Each new percentage once; the cache's 1/1 counts no bytes.
    assert_eq!(reported, [Some(40), Some(90)]);
}

/// A second download announcing its size raises the total, which must not
/// read as the install going backwards.
#[test]
fn the_reported_percentage_never_goes_down() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned())))
        .then(change(false, &[(80, 100)]))
        .then(change(false, &[(100, 100), (1, 300)]))
        .then(change(true, &[]));

    assert_eq!(run(&snapd).1, [Some(80)]);
}

/// Mounting, hooks and services come after the downloads: no percentage
/// then, and the next download resumes from where the last one left off.
#[test]
fn the_percentage_gives_way_between_downloads() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned())))
        .then(change(false, &[(60, 100)]))
        .then(change(false, &[(100, 100)]))
        .then(change(false, &[(100, 100), (10, 300)]))
        .then(change(false, &[(100, 100), (300, 300)]))
        .then(change(true, &[]));

    assert_eq!(run(&snapd).1, [Some(60), None, Some(60), None]);
}

#[test]
fn an_installed_snap_has_nothing_to_follow() {
    let snapd = Snapd::starting(Ok(None));

    assert_eq!(run(&snapd), (Ok(()), Vec::new()));
}

#[test]
fn a_refused_request_is_the_install_failing() {
    let refused = SystemConfiguratorError::snapd_authorization_denied(
        "POST /v2/snaps/myna (install, latest/edge)",
        401,
        "access denied",
    );
    let snapd = Snapd::starting(Err(refused.clone()));

    assert_eq!(run(&snapd).0, Err(refused));
}

/// snapd accepted the request (202) and the change failed after it: the
/// report names that request and what snapd said went wrong.
#[test]
fn a_failed_change_names_the_request_that_started_it() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned())))
        .then(failed("cannot perform the following tasks".into(), "Error"));

    assert_eq!(
        run(&snapd).0,
        Err(SystemConfiguratorError::snapd_execution(
            "POST /v2/snaps/myna (install, latest/edge)",
            Some(202),
            "cannot perform the following tasks",
        ))
    );
}

#[test]
fn a_change_undone_without_an_error_is_still_a_failure() {
    let snapd =
        Snapd::starting(Ok(Some("12".to_owned()))).then(failed(serde_json::Value::Null, "Undone"));

    let outcome = run(&snapd).0;

    assert!(
        matches!(&outcome, Err(SystemConfiguratorError::Execution { message, .. }) if message.contains("Undone")),
        "{outcome:?}"
    );
}

#[test]
fn a_change_that_cannot_be_read_names_the_read() {
    let snapd = (0..10).fold(
        Snapd::starting(Ok(Some("12".to_owned()))).then(change(false, &[(1, 2)])),
        |snapd, _| snapd.then_unreadable("snapd went away"),
    );

    let outcome = run(&snapd).0;

    assert_eq!(
        outcome
            .as_ref()
            .err()
            .and_then(SystemConfiguratorError::step),
        Some(&FailedStep::Snapd {
            request: "GET /v2/changes/12".to_owned(),
            http_status: None,
        })
    );
}

/// snapd restarting mid-install, as a refresh of snapd does, is no failure.
#[test]
fn a_change_read_that_fails_briefly_is_read_again() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned())))
        .then_unreadable("snapd went away")
        .then(change(false, &[(1, 2)]))
        .then_unreadable("snapd went away");
    let snapd = (0..8)
        .fold(snapd, |snapd, _| snapd.then_unreadable("snapd went away"))
        .then(change(true, &[(2, 2)]));

    assert_eq!(run(&snapd).0, Ok(()));
    assert!(snapd.reads.borrow().is_empty());
}

/// Closing the wizard stops following; snapd's change carries on.
#[test]
fn a_cancelled_follow_stops_reading() {
    let snapd = Snapd::default().then(change(false, &[(1, 2)]));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let outcome = block_on(follow_change(
        &snapd,
        "12",
        100,
        &follow(cancellation),
        &|_| {},
    ));

    assert_eq!(outcome, Err(ChangeFailure::Cancelled));
    assert_eq!(snapd.reads.borrow().len(), 1);
}

/// The wizard closed while its install ran: no failure to report.
#[test]
fn an_install_whose_follow_is_cancelled_is_cancelled() {
    let snapd = Snapd::starting(Ok(Some("12".to_owned()))).then(change(false, &[(1, 2)]));
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();

    let outcome = block_on(install(&snapd, "myna", 100, &follow(cancellation), &|_| {
        cancel.cancel()
    }));

    assert_eq!(outcome, Err(SystemConfiguratorError::Cancelled));
}
