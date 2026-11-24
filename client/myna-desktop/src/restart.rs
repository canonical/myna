//! When the daemon restarts itself to see its backend shares again.
//!
//! A bridge for snapd LP#2175592: a component install or remove on a text
//! or speech snap makes snap-update-ns remove the `backend/provider*` mount
//! points inside this process's own mount namespace, which nothing can
//! restore short of a fresh process. The share then reads empty and not
//! mounted here (`share::unmounted_shares`), the same as a model snap
//! connected after login. A share that goes so after the first check, and
//! stays so on two checks in a row, makes the daemon leave at its next idle
//! moment, and `Restart=on-failure` starts it in a namespace that has the
//! mounts. A share already so at the first check is no reason: a fresh
//! process sees it the same, and restarting for it would never end. Remove
//! this once snapd ships the fix.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

/// How often the shares are looked at.
pub const CHECK_EVERY: Duration = Duration::from_secs(5);

/// How many checks in a row must find a share lost. snap-update-ns remounts
/// the shared namespace within a second; two checks rule out reading the
/// middle of that.
pub const CHECKS_IN_A_ROW: u32 = 2;

/// Follows the unmounted shares from one check to the next.
#[derive(Debug, Default)]
pub struct RestartWatch {
    /// What the first check found, `None` before it.
    baseline: Option<BTreeSet<PathBuf>>,
    seen: u32,
}

impl RestartWatch {
    /// Record one check's unmounted shares; true once a restart is due.
    pub fn observe(&mut self, unmounted: &[PathBuf]) -> bool {
        let Some(baseline) = &self.baseline else {
            self.baseline = Some(unmounted.iter().cloned().collect());
            return false;
        };
        let lost = unmounted.iter().any(|share| !baseline.contains(share));
        self.seen = if lost { self.seen + 1 } else { 0 };
        self.seen >= CHECKS_IN_A_ROW
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shares(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn a_share_lost_on_two_checks_in_a_row_restarts() {
        let mut watch = RestartWatch::default();
        assert!(!watch.observe(&[]), "the first check only takes stock");
        assert!(
            !watch.observe(&shares(&["provider-2"])),
            "one check is not enough"
        );
        assert!(watch.observe(&shares(&["provider-2"])));
        assert!(watch.observe(&shares(&["provider-2"])), "and stays due");
    }

    #[test]
    fn a_share_seen_again_starts_the_count_over() {
        let mut watch = RestartWatch::default();
        watch.observe(&[]);
        assert!(!watch.observe(&shares(&["provider"])));
        assert!(!watch.observe(&[]));
        assert!(
            !watch.observe(&shares(&["provider"])),
            "the count started over"
        );
        assert!(watch.observe(&shares(&["provider"])));
    }

    #[test]
    fn a_share_unmounted_from_the_start_never_restarts() {
        let mut watch = RestartWatch::default();
        let stale = shares(&["provider-3"]);
        assert!(!watch.observe(&stale));
        assert!(
            (0..10).all(|_| !watch.observe(&stale)),
            "a restart would see it the same"
        );
        let both = shares(&["provider-2", "provider-3"]);
        assert!(!watch.observe(&both));
        assert!(watch.observe(&both), "a share lost since does");
    }

    #[test]
    fn nothing_lost_never_restarts() {
        let mut watch = RestartWatch::default();
        assert!((0..10).all(|_| !watch.observe(&[])));
    }
}
