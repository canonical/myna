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

/// Where a restart leaves its reason for the next process's `--status`: the
/// runtime directory, so it lasts until logout and no longer.
pub fn note_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| PathBuf::from(dir).join("myna-desktop.restart"))
}

/// The note for a restart at `unix_secs` over `shares`: the time, a tab, the
/// shares' names.
pub fn note(unix_secs: u64, shares: &[PathBuf]) -> String {
    let names: Vec<String> = shares
        .iter()
        .map(|share| {
            share.file_name().map_or_else(
                || share.display().to_string(),
                |n| n.to_string_lossy().into(),
            )
        })
        .collect();
    format!("{unix_secs}\t{}", names.join(", "))
}

/// The `--status` line for a note read at `now_secs`; `None` for no note or
/// one it cannot read.
pub fn describe(note: &str, now_secs: u64) -> Option<String> {
    let (at, shares) = note.trim_end().split_once('\t')?;
    let ago = now_secs.saturating_sub(at.parse().ok()?);
    let ago = match ago {
        0..=119 => format!("{ago} s"),
        120..=7199 => format!("{} min", ago / 60),
        _ => format!("{} h", ago / 3600),
    };
    Some(format!("{ago} ago: backend share lost here ({shares})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_note_says_when_and_which_shares() {
        let note = note(
            1_000,
            &shares(&["/var/snap/myna/x1/backend/provider-2", "provider-3"]),
        );
        assert_eq!(note, "1000\tprovider-2, provider-3");
        assert_eq!(
            describe(&note, 1_042).as_deref(),
            Some("42 s ago: backend share lost here (provider-2, provider-3)")
        );
        assert_eq!(describe(&note, 1_000 + 600).unwrap()[..6], *"10 min");
        assert_eq!(describe(&note, 1_000 + 3 * 3600).unwrap()[..3], *"3 h");
        assert_eq!(
            describe(&note, 500).unwrap()[..4],
            *"0 s ",
            "a clock gone back"
        );
    }

    #[test]
    fn an_unreadable_note_says_nothing() {
        assert_eq!(describe("", 1), None);
        assert_eq!(describe("soon\tprovider", 1), None);
        assert_eq!(describe("12 provider", 1), None);
    }

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
