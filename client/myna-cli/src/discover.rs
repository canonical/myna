//! Finding the installed snaps without being told where they are.
//!
//! Inside the myna snap the wrapper passes `$SNAP_DATA/backend`, where snapd
//! mounts every connected share. On the host that mount is invisible, but
//! each snap's own share is readable under `/var/snap/<name>/common/share`,
//! so those are searched instead.

use std::path::{Path, PathBuf};

use myna_orchestrator::backend::share::{BackendSocket, Provider};

const VAR_SNAP: &str = "/var/snap";

/// `<root>/myna*/common/share` for each that exists, in name order.
pub fn snap_share_dirs(root: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("myna"))
        .map(|e| e.path().join("common/share"))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// The speech snap the `myna` snap's `backend` plug is connected to, from
/// `snap connections myna`; `None` when snap cannot say.
fn connected_speech_snap() -> Option<String> {
    let out = std::process::Command::new("snap")
        .args(["connections", "myna"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    connected_from(&String::from_utf8_lossy(&out.stdout))
}

fn connected_from(listing: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let (_, plug, slot) = (cols.next()?, cols.next()?, cols.next()?);
        (plug == "myna:backend")
            .then(|| slot.split(':').next().map(str::to_owned))
            .flatten()
    })
}

/// One of `found` to dictate to: the only one, or the one `connected` names.
pub fn pick(found: Vec<Provider>, connected: Option<&str>) -> Result<Provider, String> {
    let names = || {
        found
            .iter()
            .filter_map(|p| p.snap_name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match found.len() {
        0 => Err(
            "no speech backend is running (looked under /var/snap/myna*/common/share); \
                  pass --socket <path> or --backend-dir <dir>"
                .into(),
        ),
        1 => Ok(found.into_iter().next().expect("one")),
        _ => {
            let chosen = connected.and_then(|name| {
                found
                    .iter()
                    .find(|p| p.snap_name.as_deref() == Some(name))
                    .cloned()
            });
            chosen.ok_or_else(|| {
                format!(
                    "several speech backends are running ({}) and none is connected to \
                     myna:backend; pass --socket <path>",
                    names()
                )
            })
        }
    }
}

/// Where to find the speech backend when no flag says: the snap's own
/// share directory inside the snap, the installed speech snaps on the host.
pub fn default_backend(root: &Path) -> Result<BackendSocket, String> {
    if let Some(data) = myna_snap_data() {
        return Ok(BackendSocket::Search(data.join("backend")));
    }
    let found: Vec<Provider> = snap_share_dirs(root)
        .into_iter()
        .filter_map(|dir| BackendSocket::Search(dir).resolve().ok())
        .collect();
    let connected = (found.len() > 1).then(connected_speech_snap).flatten();
    pick(found, connected.as_deref()).map(|p| BackendSocket::Fixed(p.socket))
}

/// `$SNAP_DATA` when this process runs inside the myna snap itself. A
/// terminal that is another snap exports its own, which says nothing here.
pub fn myna_snap_data() -> Option<PathBuf> {
    (std::env::var_os("SNAP_NAME")? == "myna")
        .then(|| std::env::var_os("SNAP_DATA").map(PathBuf::from))
        .flatten()
}

/// The host's snap root.
pub fn var_snap() -> &'static Path {
    Path::new(VAR_SNAP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &str) -> Provider {
        Provider {
            socket: PathBuf::from(format!("/{name}.sock")),
            snap_name: Some(name.into()),
        }
    }

    #[test]
    fn the_connection_is_read_from_the_listing() {
        let listing = "Interface                Plug         Slot                    Notes\n\
                       content[inference-provider]  myna:backend  myna-parakeet:provider  manual\n";
        assert_eq!(connected_from(listing).as_deref(), Some("myna-parakeet"));
        assert_eq!(connected_from("Interface Plug Slot Notes\n"), None);
        assert_eq!(connected_from(""), None);
    }

    #[test]
    fn one_backend_is_the_one_and_several_need_the_connected_one() {
        let one = pick(vec![provider("a")], None).unwrap();
        assert_eq!(one.snap_name.as_deref(), Some("a"));
        let both = || vec![provider("a"), provider("b")];
        assert_eq!(
            pick(both(), Some("b")).unwrap().snap_name.as_deref(),
            Some("b")
        );
        let err = pick(both(), None).unwrap_err();
        assert!(err.contains("a, b") && err.contains("--socket"), "{err}");
        assert!(pick(both(), Some("zzz")).is_err());
        assert!(pick(vec![], None)
            .unwrap_err()
            .contains("no speech backend"));
    }

    #[test]
    fn only_myna_snaps_with_a_share_are_searched() {
        let root = std::env::temp_dir().join(format!("myna-discover-{}", std::process::id()));
        for dir in [
            "myna-text/common/share",
            "myna-parakeet/common/share",
            "myna-whisper/common",
            "other/common/share",
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        let found = snap_share_dirs(&root);
        std::fs::remove_dir_all(&root).unwrap();
        let names: Vec<_> = found
            .iter()
            .map(|p| {
                p.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            ["myna-parakeet/common/share", "myna-text/common/share"]
        );
        assert!(snap_share_dirs(Path::new("/nonexistent")).is_empty());
    }
}
