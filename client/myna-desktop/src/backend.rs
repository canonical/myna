//! Where the backend session socket is, resolved *per utterance*.
//!
//! The socket is shared in by an inference snap over the `backend` content
//! interface, so it is not a fixed path and it is not permanently present:
//! snapd appends the slot's source basename to the target (`backend/run`, and
//! `run-2`, `run-3`, … for further connections), a `snap refresh` of the
//! backend re-creates it, and until a backend is connected there is no socket
//! at all. Resolving once at startup would therefore make the daemon's whole
//! lifetime hostage to the state of the mount at the moment the session
//! logged in.
//!
//! So resolution happens at each Press instead, and "no backend" is an error
//! the user sees on the indicator rather than a reason to exit.

use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};

/// The socket file an inference snap exports inside its shared directory.
const SOCKET_NAME: &str = "ubustt.sock";

/// Why no single backend socket could be named.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// No backend snap is connected (or none has started its session server).
    #[error("no backend is connected - install one and connect it, e.g. `sudo snap connect myna:backend myna-whisper`")]
    NotConnected,
    /// More than one is connected. Which one answers would be decided by the
    /// order they were connected, and can change when a backend is
    /// reinstalled, so this is an error rather than a guess.
    #[error("{0} backends are connected; disconnect all but one (`snap connections myna`)")]
    Ambiguous(usize),
}

/// Find the one backend socket under `dir`, looking at `dir/<entry>/ubustt.sock`
/// for every subdirectory. Missing `dir` reads as "not connected": before the
/// first `snap connect` there is no mount point at all.
pub fn resolve(dir: &Path) -> Result<PathBuf, ResolveError> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ResolveError::NotConnected),
        // A share whose mount is broken (EIO, ENOTCONN) is indistinguishable
        // from an absent one from here, and both mean the same thing to the
        // user: nothing to dictate through.
        Err(_) => return Err(ResolveError::NotConnected),
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join(SOCKET_NAME);
        if candidate
            .metadata()
            .is_ok_and(|m| m.file_type().is_socket())
        {
            found.push(candidate);
        }
    }
    match found.len() {
        0 => Err(ResolveError::NotConnected),
        1 => Ok(found.pop().expect("length checked")),
        n => Err(ResolveError::Ambiguous(n)),
    }
}

/// Either an explicit socket path (`--socket`, the dev/testbed path) or a
/// directory to resolve one out of at each Press (`--backend-dir`, how the
/// snap is wired).
#[derive(Debug, Clone)]
pub enum BackendSocket {
    /// A fixed path, used verbatim. Still allowed to be absent at startup -
    /// the connect happens per session, so a backend that starts later works.
    Fixed(PathBuf),
    /// A content-share target to search at each session start.
    Search(PathBuf),
}

impl BackendSocket {
    /// The path to connect this utterance to.
    pub fn resolve(&self) -> Result<PathBuf, ResolveError> {
        match self {
            Self::Fixed(path) => Ok(path.clone()),
            Self::Search(dir) => resolve(dir),
        }
    }

    /// What to show the user when nothing is wired up yet.
    pub fn describe(&self) -> String {
        match self {
            Self::Fixed(path) => path.display().to_string(),
            Self::Search(dir) => format!("{}/*/{SOCKET_NAME}", dir.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn socket_in(dir: &Path, name: &str) -> UnixListener {
        let sub = dir.join(name);
        std::fs::create_dir_all(&sub).expect("subdir");
        UnixListener::bind(sub.join(SOCKET_NAME)).expect("bind")
    }

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "myna-backend-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("tmpdir");
        base
    }

    /// A target that was never mounted is "not connected", not an IO error the
    /// daemon should die on.
    #[test]
    fn missing_target_is_not_connected() {
        let dir = tmpdir().join("never-mounted");
        assert!(matches!(resolve(&dir), Err(ResolveError::NotConnected)));
    }

    #[test]
    fn one_share_resolves() {
        let dir = tmpdir();
        let _held = socket_in(&dir, "run");
        assert_eq!(
            resolve(&dir).expect("resolved"),
            dir.join("run/ubustt.sock")
        );
    }

    /// Two connected backends is an error, not a coin flip: which one answers
    /// would depend on connect order and would change under a reinstall.
    #[test]
    fn two_shares_are_ambiguous() {
        let dir = tmpdir();
        let _a = socket_in(&dir, "run");
        let _b = socket_in(&dir, "run-2");
        assert!(matches!(resolve(&dir), Err(ResolveError::Ambiguous(2))));
    }

    /// A mounted-but-empty share (backend installed, session server not up
    /// yet) reads as not connected - and, because resolution is per Press,
    /// the same daemon picks the socket up once it appears.
    #[test]
    fn empty_share_is_not_connected() {
        let dir = tmpdir();
        std::fs::create_dir_all(dir.join("run")).expect("subdir");
        assert!(matches!(resolve(&dir), Err(ResolveError::NotConnected)));
    }
}
