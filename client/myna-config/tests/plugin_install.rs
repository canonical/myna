//! Installing and removing a text plugin: the request that reaches snapd,
//! the prompt count, the follow to the end and what the daemon is told.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use gtk4::glib::MainContext;
use myna_config::active_backend::SwitchPlan;
use myna_config::backend_apply::ApplyPreview;
use myna_config::command::CancellationToken;
use myna_config::domain::CommandResult;
use myna_config::plugin_catalog::ComponentRequest;
use myna_config::plugin_install::{
    dictating, install, install_route, remove, Context, Failure, Outcome, Route, SnapState,
};
use myna_config::ports::{
    DictationDaemon, FailedStep, PluginSetUp, SetUpPlan, SystemConfigurator,
    SystemConfiguratorError, SystemConfiguratorFailure,
};
use myna_config::snap_changes::{parse_change, ChangeInProgress};
use myna_config::snap_install::Follow;

#[derive(Default)]
struct Snapd {
    rest: RefCell<VecDeque<Result<Option<String>, SystemConfiguratorError>>>,
    set_ups: RefCell<VecDeque<Result<(), SystemConfiguratorError>>>,
    reads: RefCell<VecDeque<ChangeInProgress>>,
    in_progress: RefCell<VecDeque<Vec<ChangeInProgress>>>,
    calls: RefCell<Vec<String>>,
}

#[async_trait(?Send)]
impl SystemConfigurator for Snapd {
    async fn execute_backend_switch(
        &self,
        _: &SwitchPlan,
        _: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        unreachable!()
    }
    async fn restart_myna(&self, _: CancellationToken) -> Result<(), SystemConfiguratorError> {
        unreachable!()
    }
    async fn apply_backend_config(
        &self,
        _: &ApplyPreview,
        _: CancellationToken,
    ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
        unreachable!()
    }
    async fn user_daemons_enabled(&self, _: CancellationToken) -> Result<bool, String> {
        unreachable!()
    }
    async fn set_up(
        &self,
        _: &SetUpPlan,
        _: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        unreachable!()
    }
    async fn install_snap(
        &self,
        _: &str,
        _: CancellationToken,
    ) -> Result<Option<String>, SystemConfiguratorError> {
        unreachable!("a plugin never installs a bare snap")
    }
    async fn change_components(
        &self,
        snap: &str,
        components: &[String],
        install: bool,
        _: CancellationToken,
    ) -> Result<Option<String>, SystemConfiguratorError> {
        let verb = if install { "install" } else { "remove" };
        self.calls
            .borrow_mut()
            .push(format!("rest {verb} {snap} {}", components.join("+")));
        self.rest
            .borrow_mut()
            .pop_front()
            .expect("no more requests")
    }
    async fn set_up_plugin(
        &self,
        plan: &PluginSetUp,
        _: CancellationToken,
    ) -> Result<(), SystemConfiguratorError> {
        self.calls.borrow_mut().push(format!(
            "set-up {} {} connect={}",
            plan.snap,
            plan.components.join("+"),
            plan.connect
        ));
        // Pending across a few naps, so the change is read meanwhile.
        for _ in 0..3 {
            Pending(false).await;
        }
        self.set_ups
            .borrow_mut()
            .pop_front()
            .expect("no more set-ups")
    }
    async fn changes_in_progress(
        &self,
        _: CancellationToken,
    ) -> Result<Vec<myna_config::snap_changes::ChangeInProgress>, String> {
        Ok(self
            .in_progress
            .borrow_mut()
            .pop_front()
            .unwrap_or_default())
    }
    async fn snap_change(
        &self,
        id: &str,
        _: CancellationToken,
    ) -> Result<ChangeInProgress, String> {
        assert_eq!(id, "7");
        Ok(self
            .reads
            .borrow_mut()
            .pop_front()
            .expect("read after done"))
    }
}

/// Pends once, waking itself, so the racing nap is polled between.
struct Pending(bool);
impl Future for Pending {
    type Output = ();
    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

#[derive(Default)]
struct Daemon {
    state: Option<String>,
    offered: RefCell<VecDeque<Option<Vec<String>>>>,
    rescans: RefCell<u32>,
}

#[async_trait(?Send)]
impl DictationDaemon for Daemon {
    async fn state(&self) -> Result<Option<String>, String> {
        Ok(self.state.clone())
    }
    async fn offered_models(&self) -> Result<Option<Vec<String>>, String> {
        let mut offered = self.offered.borrow_mut();
        Ok(if offered.len() > 1 {
            offered.pop_front().unwrap()
        } else {
            offered.front().cloned().flatten()
        })
    }
    async fn rescan(&self) -> Result<(), String> {
        *self.rescans.borrow_mut() += 1;
        Ok(())
    }
}

fn change(ready: bool, done: u64, total: u64) -> ChangeInProgress {
    parse_change(serde_json::json!({
        "id": "7", "kind": "install-component", "ready": ready,
        "status": if ready { "Done" } else { "Doing" },
        "summary": "Install component \"model-translate-en-fr\" for snap \"myna-translation\"",
        "tasks": [{"kind": "download-component", "status": if ready {"Done"} else {"Doing"},
            "progress": {"label": "x", "done": done, "total": total}}],
    }))
    .unwrap()
}

fn request() -> ComponentRequest {
    ComponentRequest {
        snap: "myna-translation".into(),
        components: vec!["model-translate-en-fr".into()],
        models: vec!["bergamot-en-fr".into()],
        expected_bytes: 100,
    }
}

fn nap(_: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
    Box::pin(Pending(false))
}

fn follow() -> Follow<'static> {
    Follow {
        interval: Duration::ZERO,
        sleep: &nap,
        cancellation: CancellationToken::new(),
    }
}

fn block_on<T>(future: impl Future<Output = T>) -> T {
    MainContext::new().block_on(future)
}

const READY: SnapState = SnapState {
    installed: true,
    connected: true,
};

fn offering(sets: &[Option<&[&str]>]) -> Daemon {
    Daemon {
        offered: RefCell::new(
            sets.iter()
                .map(|s| s.map(|m| m.iter().map(|m| (*m).to_owned()).collect()))
                .collect(),
        ),
        ..Daemon::default()
    }
}

fn run_install(
    snapd: &Snapd,
    daemon: &Daemon,
    state: SnapState,
) -> (Result<Outcome, Failure>, Vec<Option<u8>>) {
    let follow = follow();
    let ctx = Context {
        configurator: snapd,
        daemon,
        follow: &follow,
    };
    let seen = RefCell::new(Vec::new());
    let outcome = block_on(install(&ctx, &request(), state, &|p| {
        seen.borrow_mut().push(p)
    }));
    (outcome, seen.into_inner())
}

#[test]
fn an_installed_connected_snap_takes_one_user_request_and_the_daemon_is_told() {
    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Ok(Some("7".into())));
    snapd.reads.borrow_mut().extend([
        change(false, 40, 100),
        change(false, 80, 100),
        change(true, 100, 100),
    ]);
    let daemon = offering(&[Some(&[]), Some(&["bergamot-en-fr"])]);

    let (outcome, percents) = run_install(&snapd, &daemon, READY);

    assert_eq!(outcome, Ok(Outcome::Settled));
    assert_eq!(
        *snapd.calls.borrow(),
        ["rest install myna-translation model-translate-en-fr"]
    );
    assert_eq!(percents, [Some(40), Some(80)]);
    assert_eq!(*daemon.rescans.borrow(), 1);
}

#[test]
fn a_missing_or_unconnected_snap_takes_one_root_set_up_that_connects() {
    for (state, connect) in [
        (
            SnapState {
                installed: false,
                connected: false,
            },
            true,
        ),
        (
            SnapState {
                installed: true,
                connected: false,
            },
            true,
        ),
    ] {
        let snapd = Snapd::default();
        snapd.set_ups.borrow_mut().push_back(Ok(()));
        snapd
            .in_progress
            .borrow_mut()
            .extend([vec![change(false, 10, 100)], vec![change(false, 90, 100)]]);
        let daemon = offering(&[Some(&["bergamot-en-fr"])]);

        let (outcome, percents) = run_install(&snapd, &daemon, state);

        assert_eq!(outcome, Ok(Outcome::Settled));
        assert_eq!(
            *snapd.calls.borrow(),
            [format!(
                "set-up myna-translation model-translate-en-fr connect={connect}"
            )]
        );
        assert!(percents.contains(&Some(10)), "{percents:?}");
    }
}

#[test]
fn the_route_names_one_prompt_in_every_state() {
    let state = |installed, connected| {
        install_route(SnapState {
            installed,
            connected,
        })
    };
    assert_eq!(state(true, true), Route::Rest);
    assert_eq!(state(true, false), Route::SetUp { connect: true });
    assert_eq!(state(false, false), Route::SetUp { connect: true });
    assert_eq!(state(false, true), Route::SetUp { connect: false });
}

#[test]
fn nothing_is_installed_while_a_dictation_is_active() {
    for active in ["loading", "recording", "transcribing", "finalizing"] {
        assert!(dictating(active));
        let snapd = Snapd::default();
        let daemon = Daemon {
            state: Some(active.into()),
            ..offering(&[Some(&[])])
        };
        let (outcome, _) = run_install(&snapd, &daemon, READY);
        assert_eq!(outcome, Err(Failure::Dictating));
        assert!(snapd.calls.borrow().is_empty());
        assert_eq!(*daemon.rescans.borrow(), 0);
    }
    for quiet in ["idle", "warning", "error", ""] {
        assert!(!dictating(quiet), "{quiet}");
        let snapd = Snapd::default();
        snapd.rest.borrow_mut().push_back(Ok(None));
        let daemon = Daemon {
            state: Some(quiet.into()),
            ..offering(&[Some(&["bergamot-en-fr"])])
        };
        let (outcome, _) = run_install(&snapd, &daemon, READY);
        assert_eq!(outcome, Ok(Outcome::Settled), "{quiet}");
    }
}

#[test]
fn a_dismissed_prompt_is_silent_and_a_failure_keeps_its_report() {
    let snapd = Snapd::default();
    snapd
        .rest
        .borrow_mut()
        .push_back(Err(SystemConfiguratorError::Cancelled));
    let (outcome, _) = run_install(&snapd, &offering(&[Some(&[])]), READY);
    assert_eq!(outcome, Err(Failure::Dismissed));

    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Ok(Some("7".into())));
    let done = parse_change(serde_json::json!({
        "id": "7", "kind": "install-component", "ready": true, "status": "Error",
        "err": "cannot perform: disk full", "summary": "x", "tasks": [],
    }))
    .unwrap();
    snapd.reads.borrow_mut().push_back(done);
    let (outcome, _) = run_install(&snapd, &offering(&[Some(&[])]), READY);
    let Err(Failure::Failed(SystemConfiguratorError::Execution { step, message })) = outcome else {
        panic!("{outcome:?}");
    };
    assert!(message.contains("disk full"));
    assert_eq!(
        step,
        FailedStep::Snapd {
            request: "POST /v2/snaps/myna-translation (install components)".into(),
            http_status: Some(202),
        }
    );
}

#[test]
fn a_transient_store_answer_is_retried_once() {
    let store = |code| {
        SystemConfiguratorError::snapd_execution(
            "POST /v2/snaps/myna-translation (install components)",
            Some(500),
            format!(
                "cannot fetch: got unexpected HTTP status code {code} via GET to \"https://api.snapcraft.io/x\""
            ),
        )
    };
    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Err(store(503)));
    snapd.rest.borrow_mut().push_back(Ok(Some("7".into())));
    snapd.reads.borrow_mut().push_back(change(true, 100, 100));
    let (outcome, _) = run_install(&snapd, &offering(&[Some(&["bergamot-en-fr"])]), READY);
    assert_eq!(outcome, Ok(Outcome::Settled));
    assert_eq!(snapd.calls.borrow().len(), 2);

    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Err(store(404)));
    let (outcome, _) = run_install(&snapd, &offering(&[Some(&[])]), READY);
    assert!(matches!(outcome, Err(Failure::Failed(_))));
    assert_eq!(snapd.calls.borrow().len(), 1, "404 is final");
}

#[test]
fn a_daemon_that_does_not_say_so_leaves_the_install_unconfirmed() {
    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Ok(None));
    let daemon = offering(&[Some(&[])]);
    let (outcome, _) = run_install(&snapd, &daemon, READY);
    assert_eq!(outcome, Ok(Outcome::Unconfirmed));

    let daemon = offering(&[None]);
    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Ok(None));
    let (outcome, _) = run_install(&snapd, &daemon, READY);
    assert_eq!(outcome, Ok(Outcome::Unconfirmed));
}

#[test]
fn a_remove_is_the_same_request_with_the_other_action() {
    let snapd = Snapd::default();
    snapd.rest.borrow_mut().push_back(Ok(Some("7".into())));
    snapd.reads.borrow_mut().push_back(change(true, 0, 0));
    let daemon = offering(&[Some(&["bergamot-en-fr"]), Some(&[])]);
    let follow = follow();
    let ctx = Context {
        configurator: &snapd,
        daemon: &daemon,
        follow: &follow,
    };
    let outcome = block_on(remove(&ctx, &request(), &|_| {}));
    assert_eq!(outcome, Ok(Outcome::Settled));
    assert_eq!(
        *snapd.calls.borrow(),
        ["rest remove myna-translation model-translate-en-fr"]
    );
    assert_eq!(*daemon.rescans.borrow(), 1);
}
