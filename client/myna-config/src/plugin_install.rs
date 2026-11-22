//! Installing and removing a text plugin from Settings, GTK-free: one snapd
//! request as the user per snap (one prompt) once the snap is installed and
//! connected, else one root set-up that also installs the snap and connects
//! it, so the first install of each snap still costs one prompt.
//!
//! The caller supplies the sleep between reads, as `snap_install` does.

use std::future::Future;
use std::pin::{pin, Pin};
use std::task::Poll;

use crate::plugin_catalog::ComponentRequest;
use crate::ports::{DictationDaemon, PluginSetUp, SystemConfigurator, SystemConfiguratorError};
use crate::snap_changes::pending_component_install;
use crate::snap_install::{components_request, follow_change, ChangeFailure, Follow, StoreError};

/// How many times the daemon is asked whether it offers the models, a
/// `Follow::interval` apart.
const OFFER_ATTEMPTS: u32 = 10;

/// Where a snap stands on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapState {
    pub installed: bool,
    /// `myna:backend` is connected to its `provider` slot.
    pub connected: bool,
}

/// Whether the daemon has a dictation under way, from its `State`.
pub fn dictating(state: &str) -> bool {
    matches!(
        state,
        "loading" | "recording" | "transcribing" | "finalizing"
    )
}

/// How an install reaches snapd.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// One request as the user.
    Rest,
    /// One root set-up: install, plus the connect when missing.
    SetUp { connect: bool },
}

pub fn install_route(state: SnapState) -> Route {
    if state.installed && state.connected {
        Route::Rest
    } else {
        Route::SetUp {
            connect: !state.connected,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// A dictation is under way; nothing was asked of snapd.
    Dictating,
    /// The prompt was dismissed or following was stopped: restore the
    /// control without a word.
    Dismissed,
    /// snapd or the executor failed; `Details` shows the report.
    Failed(SystemConfiguratorError),
}

impl From<SystemConfiguratorError> for Failure {
    fn from(error: SystemConfiguratorError) -> Self {
        match error {
            SystemConfiguratorError::Cancelled => Self::Dismissed,
            error => Self::Failed(error),
        }
    }
}

/// How an accepted change ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The daemon offers (or, for a remove, no longer offers) the models.
    Settled,
    /// snapd is done but the daemon did not say so in time, or is not
    /// running; it reads the models when it next starts or rescans.
    Unconfirmed,
}

pub struct Context<'a> {
    pub configurator: &'a dyn SystemConfigurator,
    pub daemon: &'a dyn DictationDaemon,
    pub follow: &'a Follow<'a>,
}

/// Install `request`'s components. `report` hears each download percentage.
pub async fn install(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    state: SnapState,
    report: &dyn Fn(Option<u8>),
) -> Result<Outcome, Failure> {
    refuse_while_dictating(ctx.daemon).await?;
    let route = install_route(state);
    let mut outcome = run_install(ctx, request, &route, report).await;
    if let Some(error) = transient_store_error(&outcome) {
        eprintln!("{}", error.evidence(&request.snap, true));
        outcome = run_install(ctx, request, &route, report).await;
        if let Some(error) = transient_store_error(&outcome) {
            eprintln!("{}", error.evidence(&request.snap, false));
        }
    }
    outcome?;
    settle(ctx, request, true).await
}

/// Remove `request`'s components, the snap staying.
pub async fn remove(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    report: &dyn Fn(Option<u8>),
) -> Result<Outcome, Failure> {
    refuse_while_dictating(ctx.daemon).await?;
    run_rest(ctx, request, false, report).await?;
    settle(ctx, request, false).await
}

async fn refuse_while_dictating(daemon: &dyn DictationDaemon) -> Result<(), Failure> {
    match daemon.state().await {
        Ok(Some(state)) if dictating(&state) => Err(Failure::Dictating),
        _ => Ok(()),
    }
}

fn transient_store_error(outcome: &Result<(), Failure>) -> Option<StoreError> {
    match outcome {
        Err(Failure::Failed(SystemConfiguratorError::Execution { message, .. })) => {
            StoreError::find(message).filter(StoreError::transient)
        }
        _ => None,
    }
}

async fn run_install(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    route: &Route,
    report: &dyn Fn(Option<u8>),
) -> Result<(), Failure> {
    match route {
        Route::Rest => run_rest(ctx, request, true, report).await,
        Route::SetUp { connect } => run_set_up(ctx, request, *connect, report).await,
    }
}

async fn run_rest(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    install: bool,
    report: &dyn Fn(Option<u8>),
) -> Result<(), Failure> {
    let Some(change_id) = ctx
        .configurator
        .change_components(
            &request.snap,
            &request.components,
            install,
            ctx.follow.cancellation.clone(),
        )
        .await?
    else {
        return Ok(());
    };
    follow_change(
        ctx.configurator,
        &change_id,
        request.expected_bytes,
        ctx.follow,
        report,
    )
    .await
    .map_err(|failure| match failure {
        ChangeFailure::Cancelled => Failure::Dismissed,
        ChangeFailure::Failed(message) => {
            Failure::Failed(SystemConfiguratorError::snapd_execution(
                components_request(&request.snap, install),
                Some(202),
                message,
            ))
        }
        ChangeFailure::Unreadable(message) => {
            Failure::Failed(SystemConfiguratorError::snapd_execution(
                format!("GET /v2/changes/{change_id}"),
                None,
                message,
            ))
        }
    })
}

/// The root set-up blocks until done; its change is read meanwhile for the
/// percentage.
async fn run_set_up(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    connect: bool,
    report: &dyn Fn(Option<u8>),
) -> Result<(), Failure> {
    let plan = PluginSetUp {
        snap: request.snap.clone(),
        components: request.components.clone(),
        connect,
    };
    let mut running = pin!(ctx
        .configurator
        .set_up_plugin(&plan, ctx.follow.cancellation.clone()));
    let (mut shown, mut highest) = (None, 0);
    loop {
        let mut nap = (ctx.follow.sleep)(ctx.follow.interval);
        match race(running.as_mut(), nap.as_mut()).await {
            Either::Left(outcome) => return outcome.map_err(Failure::from),
            Either::Right(()) => {}
        }
        if ctx.follow.cancellation.is_cancelled() {
            return Err(Failure::Dismissed);
        }
        let Ok(changes) = ctx
            .configurator
            .changes_in_progress(ctx.follow.cancellation.clone())
            .await
        else {
            continue;
        };
        let percent = pending_component_install(&changes, &request.snap)
            .and_then(|change| change.download_percent(request.expected_bytes))
            .map(|percent| percent.max(highest));
        highest = percent.unwrap_or(highest);
        if percent != shown {
            shown = percent;
            report(percent);
        }
    }
}

enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Whichever of two futures finishes first; the other is left to be polled
/// again.
async fn race<A: Future + ?Sized, B: Future + ?Sized>(
    mut a: Pin<&mut A>,
    mut b: Pin<&mut B>,
) -> Either<A::Output, B::Output> {
    std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = a.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(value));
        }
        if let Poll::Ready(value) = b.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(value));
        }
        Poll::Pending
    })
    .await
}

/// Tell the daemon to read its shares again and wait for it to offer, or
/// stop offering, every model of `request`.
async fn settle(
    ctx: &Context<'_>,
    request: &ComponentRequest,
    installed: bool,
) -> Result<Outcome, Failure> {
    if !matches!(ctx.daemon.rescan().await, Ok(())) {
        return Ok(Outcome::Unconfirmed);
    }
    for attempt in 0..OFFER_ATTEMPTS {
        if ctx.follow.cancellation.is_cancelled() {
            break;
        }
        match ctx.daemon.offered_models().await {
            Ok(Some(offered)) => {
                if request
                    .models
                    .iter()
                    .all(|model| offered.contains(model) == installed)
                {
                    return Ok(Outcome::Settled);
                }
            }
            _ => return Ok(Outcome::Unconfirmed),
        }
        if attempt + 1 < OFFER_ATTEMPTS {
            (ctx.follow.sleep)(ctx.follow.interval).await;
        }
    }
    Ok(Outcome::Unconfirmed)
}
