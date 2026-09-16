//! Where the backend session socket is, resolved *per utterance*.
//!
//! The socket is shared in by an inference snap over the `backend` content
//! interface, so it is not a fixed path and it is not permanently present:
//! snapd appends the slot's source basename to the target (`backend/provider`,
//! and `provider-2`, `provider-3`, … for further connections), a `snap refresh`
//! of the backend re-creates it, and until a backend is connected there is no
//! socket at all. Resolving once at startup would therefore make the daemon's
//! whole lifetime hostage to the state of the mount at the moment the session
//! logged in.
//!
//! So resolution happens at each Press instead, and "no backend" is an error
//! the user sees on the indicator rather than a reason to exit.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

use crate::i18n::tr;

/// The identity file every `inference-provider` share holds.
const PROVIDER_ENV: &str = "provider.env";

/// A backend socket, and the snap providing it when a share named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub socket: PathBuf,
    pub snap_name: Option<String>,
}

/// Connected shares that could not serve as the backend.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Unusable {
    /// `SNAP_NAME`s of providers with no `UNIX_SOCKET` (TCP-only).
    pub no_unix_socket: Vec<String>,
    /// `SNAP_NAME`s of providers whose socket is not there.
    pub not_serving: Vec<String>,
    /// Shares whose `provider.env` is unreadable, lacks `SNAP_NAME` or names a
    /// socket outside the share.
    pub malformed: usize,
}

impl Unusable {
    fn is_empty(&self) -> bool {
        self.no_unix_socket.is_empty() && self.not_serving.is_empty() && self.malformed == 0
    }
}

/// Why no single backend socket could be named.
///
/// `Display` renders user-facing messages through this crate's gettext domain;
/// with no .mo installed it is the identity, so the strings below double as
/// the source templates for translation.
#[derive(Debug)]
pub enum ResolveError {
    /// No usable backend is connected; carries what was connected instead.
    NotConnected(Unusable),
    /// More than one is connected, by `SNAP_NAME`. Which one answers would be
    /// decided by the order they were connected, and can change when a backend
    /// is reinstalled, so this is an error rather than a guess.
    Ambiguous(Vec<String>),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotConnected(unusable) if unusable.is_empty() => write!(
                f,
                "{}",
                tr(
                    "no backend is connected - install one and connect it, e.g. `sudo snap connect myna:backend myna-whisper`"
                )
            ),
            ResolveError::NotConnected(unusable) => {
                let mut parts = Vec::new();
                for name in &unusable.no_unix_socket {
                    parts.push(
                        tr("%s is connected but offers no Unix socket").replace("%s", name),
                    );
                }
                for name in &unusable.not_serving {
                    parts.push(
                        tr("%s is connected but its server is not running")
                            .replace("%s", name),
                    );
                }
                if unusable.malformed > 0 {
                    parts.push(
                        tr("connected shares with an unreadable provider.env: %s")
                            .replace("%s", &unusable.malformed.to_string()),
                    );
                }
                write!(f, "{}", parts.join("; "))
            }
            ResolveError::Ambiguous(names) => write!(
                f,
                "{}",
                tr(
                    "%1$s backends are connected (%2$s); disconnect all but one (`snap connections myna`)"
                )
                .replace("%1$s", &names.len().to_string())
                .replace("%2$s", &names.join(", "))
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Find the one backend socket under `dir`, reading `dir/<entry>/provider.env`
/// for every subdirectory in name order. Missing `dir` reads as "not
/// connected": before the first `snap connect` there is no mount point at all.
pub fn resolve(dir: &Path) -> Result<Provider, ResolveError> {
    let mut shares: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        // Absent before the first `snap connect`; a broken mount (EIO,
        // ENOTCONN) means the same thing to the user: nothing to dictate
        // through.
        Err(_) => return Err(ResolveError::NotConnected(Unusable::default())),
    };
    shares.retain(|path| path.is_dir());
    shares.sort();

    let mut found = Vec::new();
    let mut unusable = Unusable::default();
    for share in shares {
        let text = match std::fs::read_to_string(share.join(PROVIDER_ENV)) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                unusable.malformed += 1;
                continue;
            }
        };
        let Some(env) = parse_env(&text) else {
            unusable.malformed += 1;
            continue;
        };
        let Some(snap_name) = env.get("SNAP_NAME").filter(|name| !name.is_empty()) else {
            unusable.malformed += 1;
            continue;
        };
        let snap_name = snap_name.clone();
        let Some(relative) = env.get("UNIX_SOCKET").filter(|s| !s.is_empty()) else {
            unusable.no_unix_socket.push(snap_name);
            continue;
        };
        let relative = Path::new(relative);
        if !relative
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        {
            unusable.malformed += 1;
            continue;
        }
        let socket = share.join(relative);
        if socket.metadata().is_ok_and(|m| m.file_type().is_socket()) {
            found.push(Provider {
                socket,
                snap_name: Some(snap_name),
            });
        } else {
            unusable.not_serving.push(snap_name);
        }
    }

    match found.len() {
        0 => Err(ResolveError::NotConnected(unusable)),
        1 => Ok(found.pop().expect("length checked")),
        _ => Err(ResolveError::Ambiguous(
            found.into_iter().filter_map(|p| p.snap_name).collect(),
        )),
    }
}

/// A `provider.env`: `KEY=value` lines, blank lines and `#` comments ignored,
/// one pair of matching quotes stripped. No interpolation, no escapes.
fn parse_env(text: &str) -> Option<HashMap<String, String>> {
    let mut env = HashMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let value = value.trim();
        let unquoted = ['"', '\'']
            .iter()
            .find_map(|&q| value.strip_prefix(q).and_then(|rest| rest.strip_suffix(q)))
            .unwrap_or(value);
        env.insert(key.to_string(), unquoted.to_string());
    }
    Some(env)
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
    /// The socket to connect this utterance to.
    pub fn resolve(&self) -> Result<Provider, ResolveError> {
        match self {
            Self::Fixed(path) => Ok(Provider {
                socket: path.clone(),
                snap_name: None,
            }),
            Self::Search(dir) => resolve(dir),
        }
    }

    /// What to show the user when nothing is wired up yet.
    pub fn describe(&self) -> String {
        match self {
            Self::Fixed(path) => path.display().to_string(),
            Self::Search(dir) => format!("{}/*/{PROVIDER_ENV}", dir.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

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

    fn share(dir: &Path, name: &str, env: &str) -> PathBuf {
        let sub = dir.join(name);
        std::fs::create_dir_all(&sub).expect("subdir");
        std::fs::write(sub.join(PROVIDER_ENV), env).expect("provider.env");
        sub
    }

    fn env(snap: &str) -> String {
        format!("SNAP_NAME={snap}\nSNAP_INSTANCE_NAME={snap}\nUNIX_SOCKET=myna.sock\n")
    }

    /// A share laid out as myna-server writes it: provider.env and a live socket.
    fn serving(dir: &Path, name: &str, snap: &str) -> UnixListener {
        let sub = share(dir, name, &env(snap));
        UnixListener::bind(sub.join("myna.sock")).expect("bind")
    }

    fn not_connected(result: Result<Provider, ResolveError>) -> Unusable {
        match result {
            Err(ResolveError::NotConnected(unusable)) => unusable,
            other => panic!("expected NotConnected, got {other:?}"),
        }
    }

    /// A target that was never mounted is "not connected", not an IO error the
    /// daemon should die on.
    #[test]
    fn missing_target_is_not_connected() {
        let dir = tmpdir().join("never-mounted");
        assert_eq!(not_connected(resolve(&dir)), Unusable::default());
    }

    #[test]
    fn one_provider_resolves_to_its_socket_and_snap() {
        let dir = tmpdir();
        let _held = serving(&dir, "provider", "myna-parakeet");
        assert_eq!(
            resolve(&dir).expect("resolved"),
            Provider {
                socket: dir.join("provider/myna.sock"),
                snap_name: Some("myna-parakeet".into()),
            }
        );
    }

    /// The socket alone is not a provider: a share is identified by its
    /// provider.env, so an old-style share reads as nothing connected.
    #[test]
    fn socket_without_provider_env_is_not_connected() {
        let dir = tmpdir();
        std::fs::create_dir_all(dir.join("run")).expect("subdir");
        let _held = UnixListener::bind(dir.join("run/myna.sock")).expect("bind");
        assert_eq!(not_connected(resolve(&dir)), Unusable::default());
    }

    #[test]
    fn files_beside_the_shares_are_ignored() {
        let dir = tmpdir();
        std::fs::write(dir.join(PROVIDER_ENV), env("stray")).expect("file");
        let _held = serving(&dir, "provider", "myna-whisper");
        assert_eq!(
            resolve(&dir).expect("resolved").snap_name.as_deref(),
            Some("myna-whisper")
        );
    }

    /// An LLM snap on the same content id shares over TCP only. It is named in
    /// the message rather than silently missing from it.
    #[test]
    fn tcp_only_provider_is_skipped_and_named() {
        let dir = tmpdir();
        share(
            &dir,
            "provider",
            "SNAP_NAME=smollm2\nSNAP_INSTANCE_NAME=smollm2\nOPENAI_BASE_URL=http://localhost:8080/v1\n",
        );
        share(&dir, "provider-2", "SNAP_NAME=gemma\nUNIX_SOCKET=\n");
        let err = resolve(&dir).expect_err("no unix socket");
        assert_eq!(
            err.to_string(),
            "smollm2 is connected but offers no Unix socket; gemma is connected but offers no Unix socket"
        );
        assert_eq!(
            not_connected(Err(err)),
            Unusable {
                no_unix_socket: vec!["smollm2".into(), "gemma".into()],
                ..Unusable::default()
            }
        );
    }

    /// A mounted share whose server is not up yet reads as not connected - and,
    /// because resolution is per Press, the same daemon picks the socket up
    /// once it appears.
    #[test]
    fn provider_without_socket_is_not_serving() {
        let dir = tmpdir();
        share(&dir, "provider", &env("myna-parakeet"));
        let plain = share(&dir, "provider-2", &env("myna-whisper"));
        std::fs::write(plain.join("myna.sock"), "").expect("regular file");
        let err = resolve(&dir).expect_err("not serving");
        assert_eq!(
            err.to_string(),
            "myna-parakeet is connected but its server is not running; \
             myna-whisper is connected but its server is not running"
        );
        assert_eq!(
            not_connected(Err(err)).not_serving,
            vec!["myna-parakeet".to_string(), "myna-whisper".to_string()]
        );
    }

    #[test]
    fn malformed_provider_env_is_skipped_and_counted() {
        let dir = tmpdir();
        share(
            &dir,
            "provider",
            "SNAP_INSTANCE_NAME=x\nUNIX_SOCKET=myna.sock\n",
        );
        share(
            &dir,
            "provider-2",
            "SNAP_NAME=myna-a\nthis is not an assignment\n",
        );
        share(&dir, "provider-3", "SNAP_NAME=\nUNIX_SOCKET=myna.sock\n");
        std::fs::create_dir_all(dir.join("provider-4").join(PROVIDER_ENV)).expect("dir as file");
        let err = resolve(&dir).expect_err("all malformed");
        assert_eq!(
            err.to_string(),
            "connected shares with an unreadable provider.env: 4"
        );
        assert_eq!(not_connected(Err(err)).malformed, 4);

        let _held = serving(&dir, "provider-5", "myna-parakeet");
        assert_eq!(
            resolve(&dir).expect("malformed never fatal").socket,
            dir.join("provider-5/myna.sock")
        );
    }

    #[test]
    fn unix_socket_outside_the_share_is_rejected() {
        let dir = tmpdir();
        std::fs::create_dir_all(dir.join("elsewhere")).expect("subdir");
        let _outside = UnixListener::bind(dir.join("elsewhere/myna.sock")).expect("bind");
        share(
            &dir,
            "provider",
            "SNAP_NAME=a\nUNIX_SOCKET=../elsewhere/myna.sock\n",
        );
        let absolute = dir.join("elsewhere/myna.sock");
        share(
            &dir,
            "provider-2",
            &format!("SNAP_NAME=b\nUNIX_SOCKET={}\n", absolute.display()),
        );
        assert_eq!(
            not_connected(resolve(&dir)),
            Unusable {
                malformed: 2,
                ..Unusable::default()
            }
        );
    }

    #[test]
    fn socket_in_a_subdirectory_of_the_share_resolves() {
        let dir = tmpdir();
        let sub = share(
            &dir,
            "provider",
            "SNAP_NAME=a\nUNIX_SOCKET=./run/myna.sock\n",
        );
        std::fs::create_dir_all(sub.join("run")).expect("run");
        let _held = UnixListener::bind(sub.join("run/myna.sock")).expect("bind");
        assert_eq!(
            resolve(&dir).expect("resolved").socket,
            sub.join("./run/myna.sock")
        );
    }

    /// Two connected backends is an error, not a coin flip: which one answers
    /// would depend on connect order and would change under a reinstall.
    #[test]
    fn two_providers_are_ambiguous() {
        let dir = tmpdir();
        let _a = serving(&dir, "provider", "myna-parakeet");
        let _b = serving(&dir, "provider-2", "myna-whisper");
        let err = resolve(&dir).expect_err("ambiguous");
        assert_eq!(
            err.to_string(),
            "2 backends are connected (myna-parakeet, myna-whisper); \
             disconnect all but one (`snap connections myna`)"
        );
        assert!(matches!(err, ResolveError::Ambiguous(names) if names.len() == 2));
    }

    /// Shares are read in name order, so messages do not reshuffle with the
    /// filesystem's directory order.
    #[test]
    fn shares_are_read_in_name_order() {
        let dir = tmpdir();
        let order = [
            "provider-3",
            "provider",
            "provider-10",
            "provider-2",
            "provider-4",
        ];
        let _held: Vec<_> = order
            .iter()
            .map(|name| serving(&dir, name, &format!("snap-{name}")))
            .collect();
        match resolve(&dir) {
            Err(ResolveError::Ambiguous(names)) => assert_eq!(
                names,
                [
                    "snap-provider",
                    "snap-provider-10",
                    "snap-provider-2",
                    "snap-provider-3",
                    "snap-provider-4"
                ]
            ),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn no_backend_message_is_the_install_hint() {
        assert!(ResolveError::NotConnected(Unusable::default())
            .to_string()
            .starts_with("no backend is connected - install one"));
    }

    #[test]
    fn env_parsing() {
        let parsed = parse_env(
            "# written by myna-server\n\n  SNAP_NAME = myna-whisper \n\
             DOUBLE=\"a b\"\nSINGLE='c'\nMISMATCHED=\"d'\nLONE=\"\nURL=http://h/?x=1\nEMPTY=\n",
        )
        .expect("parses");
        assert_eq!(parsed["SNAP_NAME"], "myna-whisper");
        assert_eq!(parsed["DOUBLE"], "a b");
        assert_eq!(parsed["SINGLE"], "c");
        assert_eq!(parsed["MISMATCHED"], "\"d'");
        assert_eq!(parsed["LONE"], "\"");
        assert_eq!(parsed["URL"], "http://h/?x=1");
        assert_eq!(parsed["EMPTY"], "");
        assert_eq!(parsed.len(), 7);
        assert_eq!(parse_env("NO_EQUALS\n"), None);
        assert_eq!(parse_env("=value\n"), None);
    }

    #[test]
    fn fixed_socket_is_used_verbatim() {
        let fixed = BackendSocket::Fixed(PathBuf::from("/run/x.sock"));
        assert_eq!(
            fixed.resolve().expect("fixed"),
            Provider {
                socket: PathBuf::from("/run/x.sock"),
                snap_name: None,
            }
        );
        assert_eq!(fixed.describe(), "/run/x.sock");
        assert_eq!(
            BackendSocket::Search(PathBuf::from("/var/snap/myna/x1/backend")).describe(),
            "/var/snap/myna/x1/backend/*/provider.env"
        );
    }
}
