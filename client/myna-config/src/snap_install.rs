//! Installing a snap as Install more models does: one request to snapd as the
//! user, whose polkit prompt is the only question, then its change followed
//! to the end. A model's component download runs inside that same change, so
//! following it covers the model too.
//!
//! GTK-free: the caller supplies the sleep between reads.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::command::CancellationToken;
use crate::ports::{SystemConfigurator, SystemConfiguratorError};

/// The only channel both Myna and its backends are published to.
pub const INSTALL_CHANNEL: &str = "latest/edge";

/// How a failure report names the request that installs `snap`.
pub fn install_request(snap: &str) -> String {
    format!("POST /v2/snaps/{snap} (install, {INSTALL_CHANNEL})")
}

/// Failed reads in a row, a second apart, that end following: snapd
/// restarting mid-install is not the install failing.
const UNREADABLE_READS: u32 = 10;

/// How a change is followed.
pub struct Follow<'a> {
    pub interval: Duration,
    pub sleep: &'a dyn Fn(Duration) -> Pin<Box<dyn Future<Output = ()>>>,
    pub cancellation: CancellationToken,
}

/// Why following a change ended short of success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeFailure {
    /// Stopped following; snapd's change carries on.
    Cancelled,
    /// snapd finished the change without doing it.
    Failed(String),
    /// The change could not be read.
    Unreadable(String),
}

/// Install `snap` and follow its change to the end. `report` hears each new
/// download percentage, measured against at least `expected_bytes`, which
/// never goes down, and none while no download runs.
pub async fn install(
    configurator: &dyn SystemConfigurator,
    snap: &str,
    expected_bytes: u64,
    follow: &Follow<'_>,
    report: &dyn Fn(Option<u8>),
) -> Result<(), SystemConfiguratorError> {
    let Some(change_id) = configurator
        .install_snap(snap, follow.cancellation.clone())
        .await?
    else {
        return Ok(());
    };
    follow_change(configurator, &change_id, expected_bytes, follow, report)
        .await
        .map_err(|failure| match failure {
            ChangeFailure::Cancelled => SystemConfiguratorError::Cancelled,
            ChangeFailure::Failed(message) => {
                SystemConfiguratorError::snapd_execution(install_request(snap), Some(202), message)
            }
            ChangeFailure::Unreadable(message) => SystemConfiguratorError::snapd_execution(
                format!("GET /v2/changes/{change_id}"),
                None,
                message,
            ),
        })
}

/// Follow change `change_id` until snapd is done with it, whoever started it.
pub async fn follow_change(
    configurator: &dyn SystemConfigurator,
    change_id: &str,
    expected_bytes: u64,
    follow: &Follow<'_>,
    report: &dyn Fn(Option<u8>),
) -> Result<(), ChangeFailure> {
    let mut shown = None;
    let mut highest = 0;
    let mut unreadable = 0;
    loop {
        if follow.cancellation.is_cancelled() {
            return Err(ChangeFailure::Cancelled);
        }
        let change = match configurator
            .snap_change(change_id, follow.cancellation.clone())
            .await
        {
            Ok(change) => change,
            Err(message) => {
                unreadable += 1;
                if unreadable == UNREADABLE_READS {
                    return Err(ChangeFailure::Unreadable(message));
                }
                (follow.sleep)(follow.interval).await;
                continue;
            }
        };
        unreadable = 0;
        if change.ready() {
            return match (change.err(), change.status()) {
                (Some(err), _) => Err(ChangeFailure::Failed(err.to_owned())),
                (None, "Done") => Ok(()),
                (None, status) => Err(ChangeFailure::Failed(format!(
                    "snapd ended the change as {status}"
                ))),
            };
        }
        let percent = change
            .download_percent(expected_bytes)
            .map(|percent| percent.max(highest));
        highest = percent.unwrap_or(highest);
        if percent != shown {
            shown = percent;
            report(percent);
        }
        (follow.sleep)(follow.interval).await;
    }
}
