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

use myna_core::provider_env::{
    parse_env, DeclaredRole, PROVIDER_ENV_FILENAME as PROVIDER_ENV, SPEECH_SOCKET_FILENAME,
};

use crate::i18n::tr;

/// A backend socket, and the snap providing it when a share named one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    pub socket: PathBuf,
    pub snap_name: Option<String>,
}

/// A connected share offering a live `myna-text-v1` Unix socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextProvider {
    pub socket: PathBuf,
    pub snap_name: String,
}

/// Snaps whose text shares Myna binds. Widening the list is a release. The
/// name is the share's own `SNAP_NAME`, which only the snap's provider hook
/// writes; a sideload can spoof it on its owner's machine only.
pub const CURATED_TEXT_SNAPS: &[&str] = &["myna-text", "myna-translation"];

impl TextProvider {
    /// Whether the providing snap is on [`CURATED_TEXT_SNAPS`].
    pub fn is_curated(&self) -> bool {
        CURATED_TEXT_SNAPS.contains(&self.snap_name.as_str())
    }
}

/// Connected text / LLM shares discovered under the `backend` directory.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TextShares {
    pub providers: Vec<TextProvider>,
    pub unusable: Unusable,
}

impl TextShares {
    /// Whether a share was connected after this process started, so only a
    /// fresh daemon process can see its mount.
    pub fn needs_restart(&self) -> bool {
        self.unusable.unmounted > 0
    }
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
    /// Shares connected after this process started, whose mount it cannot
    /// see: snapd mounts a new connection in the snap's mount namespace, but
    /// each app of a snap with user mounts runs in a namespace of its own,
    /// which only shows the empty mount point.
    pub unmounted: usize,
}

impl Unusable {
    pub fn is_empty(&self) -> bool {
        self.no_unix_socket.is_empty()
            && self.not_serving.is_empty()
            && self.malformed == 0
            && self.unmounted == 0
    }
}

/// Why no single backend socket could be named.
///
/// [`ResolveError::headline`] is what the user is told, translated through
/// this crate's gettext domain; `Display` is the untranslated detail, which
/// names the models involved. Diagnostics shows it, so it speaks of models in
/// plain words and names no command, plug or file: Myna Settings is where a
/// model is installed and connected, and [`resolve`] logs the internals.
#[derive(Debug)]
pub enum ResolveError {
    /// No usable backend is connected; carries what was connected instead.
    NotConnected(Unusable),
    /// More than one is connected, by `SNAP_NAME`. Which one answers would be
    /// decided by the order they were connected, and can change when a backend
    /// is reinstalled, so this is an error rather than a guess.
    Ambiguous(Vec<String>),
}

impl ResolveError {
    /// The short, translated message the user sees. With several unusable
    /// shares, the most likely and most fixable cause wins: an installed model
    /// whose server is down, then one that cannot serve dictation at all.
    pub fn headline(&self) -> String {
        match self {
            ResolveError::NotConnected(unusable) if unusable.is_empty() => {
                tr("Model not connected")
            }
            ResolveError::NotConnected(unusable) if unusable.unmounted > 0 => {
                tr("Model connecting… Retry shortly")
            }
            ResolveError::NotConnected(unusable) if !unusable.not_serving.is_empty() => {
                tr("Model not running")
            }
            ResolveError::NotConnected(_) => tr("Model not compatible"),
            ResolveError::Ambiguous(_) => tr("Several models connected"),
        }
    }
}

impl ResolveError {
    /// Whether only a fresh process can see the backend: one was connected
    /// after this one started (see [`Unusable::unmounted`]). Nothing failed,
    /// so the headline is a notice, not an error.
    pub fn needs_restart(&self) -> bool {
        matches!(self, ResolveError::NotConnected(unusable) if unusable.unmounted > 0)
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotConnected(unusable) if unusable.is_empty() => {
                write!(f, "no model is connected")
            }
            ResolveError::NotConnected(unusable) => {
                let mut parts = Vec::new();
                for name in &unusable.no_unix_socket {
                    parts.push(format!("{name} is connected but cannot serve dictation"));
                }
                for name in &unusable.not_serving {
                    parts.push(format!("{name} is connected but not running"));
                }
                match unusable.malformed {
                    0 => {}
                    1 => parts.push("a connected model could not be identified".to_string()),
                    n => parts.push(format!("{n} connected models could not be identified")),
                }
                if unusable.unmounted > 0 {
                    parts.push("a model was connected after Dictation started".to_string());
                }
                write!(f, "{}", parts.join("; "))
            }
            ResolveError::Ambiguous(names) => write!(
                f,
                "{} models are connected ({}); connect only one",
                names.len(),
                names.join(", ")
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Find the one speech backend socket under `dir`, reading
/// `dir/<entry>/provider.env` for every subdirectory in name order. Missing
/// `dir` reads as "not connected": before the first `snap connect` there is no
/// mount point at all.
pub fn resolve(dir: &Path) -> Result<Provider, ResolveError> {
    resolve_speech(dir)
}

/// Resolve the active speech provider under `dir`, ignoring connected text or
/// LLM shares so simultaneous speech + text connections never collide.
pub fn resolve_speech(dir: &Path) -> Result<Provider, ResolveError> {
    resolve_with(dir, is_mount_point)
}

/// Discover all connected text and LLM shares under `dir`, preserving
/// [`Unusable::unmounted`] when a new share was connected after daemon start.
pub fn resolve_text_shares(dir: &Path) -> TextShares {
    resolve_text_shares_with(dir, is_mount_point)
}

fn resolve_text_shares_with(dir: &Path, mounted: impl Fn(&Path) -> bool) -> TextShares {
    enumerate_with(dir, mounted).text
}

/// [`resolve_speech`], asking `mounted` whether an empty share is a mount point.
fn resolve_with(dir: &Path, mounted: impl Fn(&Path) -> bool) -> Result<Provider, ResolveError> {
    let mut partitioned = enumerate_with(dir, mounted);
    match partitioned.speech_providers.len() {
        0 => Err(ResolveError::NotConnected(partitioned.speech_unusable)),
        1 => Ok(partitioned.speech_providers.pop().expect("length checked")),
        _ => Err(ResolveError::Ambiguous(
            partitioned
                .speech_providers
                .into_iter()
                .filter_map(|p| p.snap_name)
                .collect(),
        )),
    }
}

#[derive(Default)]
struct PartitionedShares {
    speech_providers: Vec<Provider>,
    speech_unusable: Unusable,
    text: TextShares,
}

#[derive(Clone, Copy)]
enum ShareRole {
    Speech,
    Text,
    Other,
}

/// What a share's `provider.env` makes of it: a speech backend, a text
/// provider, another role, or `None` when Myna does not bind it (no role and
/// no sign of a legacy speech snap). One pure function: the directories, the
/// sockets and the counters are the caller's.
fn role_of(
    snap_name: &str,
    unix_socket: Option<&str>,
    env: &HashMap<String, String>,
) -> Option<ShareRole> {
    match DeclaredRole::from_env(env) {
        Some(DeclaredRole::Speech) => Some(ShareRole::Speech),
        Some(DeclaredRole::Text) => Some(ShareRole::Text),
        Some(DeclaredRole::Other) => Some(ShareRole::Other),
        None if is_legacy_speech(snap_name, unix_socket) => Some(ShareRole::Speech),
        None => None,
    }
}

/// A share's `provider.env`, or `None` and the reason it is not one. An
/// empty directory outside this process's mounts is a connection it missed.
fn env_of(
    share: &Path,
    mounted: &impl Fn(&Path) -> bool,
    out: &mut PartitionedShares,
) -> Option<(String, HashMap<String, String>)> {
    let text = match std::fs::read_to_string(share.join(PROVIDER_ENV)) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if is_empty_dir(share) && !mounted(share) {
                myna_core::info_log!(
                    "backend",
                    "{} is empty and not mounted in this namespace: connected after start",
                    share.display()
                );
                out.speech_unusable.unmounted += 1;
                out.text.unusable.unmounted += 1;
            }
            return None;
        }
        Err(e) => {
            malformed_both(out, share, &format!("unreadable: {e}"));
            return None;
        }
    };
    let Some(env) = parse_env(&text) else {
        malformed_both(out, share, "not KEY=value lines");
        return None;
    };
    match non_empty(&env, "SNAP_NAME") {
        Some(snap_name) => Some((snap_name.to_string(), env)),
        None => {
            malformed_both(out, share, "no SNAP_NAME");
            None
        }
    }
}

/// The live socket a share names, or `None` after counting why not.
fn socket_of(
    share: &Path,
    snap_name: &str,
    unix_socket: Option<&str>,
    unusable: &mut Unusable,
) -> Option<PathBuf> {
    let Some(relative) = unix_socket else {
        myna_core::info_log!("backend", "{snap_name}: {PROVIDER_ENV} has no UNIX_SOCKET");
        unusable.no_unix_socket.push(snap_name.to_owned());
        return None;
    };
    let Some(socket) = resolve_in_share(share, relative) else {
        malformed(unusable, share, "UNIX_SOCKET outside the share");
        return None;
    };
    if is_live_socket(&socket) {
        return Some(socket);
    }
    myna_core::info_log!("backend", "{snap_name}: no socket at {}", socket.display());
    unusable.not_serving.push(snap_name.to_owned());
    None
}

/// Every share's role, socket and whether it serves, in one pass.
fn enumerate_with(dir: &Path, mounted: impl Fn(&Path) -> bool) -> PartitionedShares {
    let mut out = PartitionedShares::default();
    for share in shares_in(dir) {
        let Some((snap_name, env)) = env_of(&share, &mounted, &mut out) else {
            continue;
        };
        let unix_socket = non_empty(&env, "UNIX_SOCKET");
        match role_of(&snap_name, unix_socket, &env) {
            Some(ShareRole::Speech) => {
                if let Some(socket) =
                    socket_of(&share, &snap_name, unix_socket, &mut out.speech_unusable)
                {
                    out.speech_providers.push(Provider {
                        socket,
                        snap_name: Some(snap_name),
                    });
                }
            }
            Some(ShareRole::Text) => {
                if let Some(socket) =
                    socket_of(&share, &snap_name, unix_socket, &mut out.text.unusable)
                {
                    out.text.providers.push(TextProvider { socket, snap_name });
                }
            }
            Some(ShareRole::Other) => myna_core::info_log!(
                "backend",
                "{snap_name}: {PROVIDER_ENV} declares an unsupported MYNA_ROLE"
            ),
            None => myna_core::info_log!(
                "backend",
                "{snap_name}: no MYNA_ROLE and no speech socket, ignored"
            ),
        }
    }
    out
}

/// The share directories under `dir`, sorted; none before the first
/// `snap connect`, and a broken mount (EIO, ENOTCONN) means the same thing.
fn shares_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut shares: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    shares.retain(|path| path.is_dir());
    shares.sort();
    shares
}

fn non_empty<'a>(env: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    env.get(key).map(String::as_str).filter(|s| !s.is_empty())
}

fn is_legacy_speech(snap_name: &str, unix_socket: Option<&str>) -> bool {
    let matches_speech_socket = unix_socket
        .and_then(|s| Path::new(s).file_name())
        .is_some_and(|name| name == SPEECH_SOCKET_FILENAME);
    matches_speech_socket
        || myna_core::language::ModelFamily::from_snap_name(snap_name).is_some()
        || snap_name == "myna-fake-backend"
}

fn resolve_in_share(share: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then(|| share.join(relative))
}

fn is_live_socket(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.file_type().is_socket())
}

fn malformed(unusable: &mut Unusable, share: &Path, why: &str) {
    myna_core::info_log!("backend", "{}/{PROVIDER_ENV}: {why}", share.display());
    unusable.malformed += 1;
}

fn malformed_both(out: &mut PartitionedShares, share: &Path, why: &str) {
    myna_core::info_log!("backend", "{}/{PROVIDER_ENV}: {why}", share.display());
    out.speech_unusable.malformed += 1;
    out.text.unusable.malformed += 1;
}

/// The shares under `dir` that are empty and not mounted in this process:
/// connected after it started, or unmounted under it (snapd LP#2175592).
/// Only a fresh process can see them.
pub fn unmounted_shares(dir: &Path) -> Vec<PathBuf> {
    unmounted_shares_with(dir, is_mount_point)
}

fn unmounted_shares_with(dir: &Path, mounted: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut shares: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && is_empty_dir(path) && !mounted(path))
        .collect();
    shares.sort();
    shares
}

fn is_empty_dir(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Whether `path` is a mount point in this process's mount namespace.
fn is_mount_point(path: &Path) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    std::fs::read_to_string("/proc/self/mountinfo")
        .is_ok_and(|table| mount_points(&table).any(|point| point == path))
}

/// The mount points `/proc/self/mountinfo` lists, its octal escapes decoded.
fn mount_points(table: &str) -> impl Iterator<Item = PathBuf> + '_ {
    table
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(unescape_mountinfo)
}

fn unescape_mountinfo(field: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let octal = bytes
            .get(i + 1..i + 4)
            .filter(|digits| bytes[i] == b'\\' && digits.iter().all(|d| (b'0'..=b'7').contains(d)))
            .and_then(|digits| u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok());
        match octal {
            Some(byte) => {
                out.push(byte);
                i += 4;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(std::ffi::OsString::from_vec(out))
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
    /// The backend a `--socket <path>` / `--backend-dir <dir>` pair names, or
    /// `None` when neither was given.
    pub fn from_flags(
        socket: Option<PathBuf>,
        backend_dir: Option<PathBuf>,
    ) -> Result<Option<Self>, String> {
        match (socket, backend_dir) {
            (Some(_), Some(_)) => {
                Err("--socket and --backend-dir are alternatives (pick one)".into())
            }
            (Some(path), None) => Ok(Some(Self::Fixed(path))),
            (None, Some(dir)) => Ok(Some(Self::Search(dir))),
            (None, None) => Ok(None),
        }
    }

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

    #[test]
    fn only_the_listed_snaps_are_curated() {
        let provider = |name: &str| TextProvider {
            socket: "/s".into(),
            snap_name: name.into(),
        };
        for name in ["myna-text", "myna-translation"] {
            assert!(provider(name).is_curated(), "{name}");
        }
        for name in ["gemma4", "qwen3", "myna-text2", "", "Gemma4"] {
            assert!(!provider(name).is_curated(), "{name}");
        }
    }
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
    fn an_empty_share_this_process_cannot_see_mounted_needs_a_restart() {
        let dir = tmpdir();
        std::fs::create_dir_all(dir.join("provider")).expect("mount point");
        let error = resolve_with(&dir, |_| false).expect_err("nothing to connect to");
        assert!(error.needs_restart(), "{error:?}");
        assert_eq!(
            not_connected(Err(error)),
            Unusable {
                unmounted: 1,
                ..Unusable::default()
            }
        );
        let error = resolve_with(&dir, |_| false).expect_err("still nothing");
        assert_eq!(error.headline(), "Model connecting… Retry shortly");
        assert_eq!(
            error.to_string(),
            "a model was connected after Dictation started"
        );
    }

    /// Mounted but empty is a backend that has not written its share yet.
    #[test]
    fn provider_without_socket_is_not_serving() {
        let dir = tmpdir();
        share(&dir, "provider", &env("myna-parakeet"));
        let plain = share(&dir, "provider-2", &env("myna-whisper"));
        std::fs::write(plain.join("myna.sock"), "").expect("regular file");
        let err = resolve(&dir).expect_err("not serving");
        assert_eq!(
            err.to_string(),
            "myna-parakeet is connected but not running; myna-whisper is connected but not running"
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
            "4 connected models could not be identified"
        );
        assert_eq!(not_connected(Err(err)).malformed, 4);
        assert_eq!(resolve_text_shares(&dir).unusable.malformed, 4);

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
    fn two_providers_are_ambiguous() {
        let dir = tmpdir();
        let _a = serving(&dir, "provider", "myna-parakeet");
        let _b = serving(&dir, "provider-2", "myna-whisper");
        let err = resolve(&dir).expect_err("ambiguous");
        assert_eq!(
            err.to_string(),
            "2 models are connected (myna-parakeet, myna-whisper); connect only one"
        );
        assert!(matches!(err, ResolveError::Ambiguous(names) if names.len() == 2));
    }

    /// Shares are read in name order, so messages do not reshuffle with the
    /// filesystem's directory order.
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
    fn speech_and_text_shares_coexist_without_ambiguity() {
        let dir = tmpdir();
        let s1 = share(
            &dir,
            "provider",
            "SNAP_NAME=myna-parakeet\nSNAP_INSTANCE_NAME=myna-parakeet\nUNIX_SOCKET=myna.sock\nMYNA_ROLE=speech\n",
        );
        let _speech = UnixListener::bind(s1.join("myna.sock")).expect("bind speech");
        let s2 = share(
            &dir,
            "provider-2",
            "SNAP_NAME=myna-text\nSNAP_INSTANCE_NAME=myna-text\nUNIX_SOCKET=myna.sock\nMYNA_ROLE=text\n",
        );
        let _text = UnixListener::bind(s2.join("myna.sock")).expect("bind text");
        for (name, env) in [
            (
                "provider-3",
                "SNAP_NAME=gemma4\nSNAP_INSTANCE_NAME=gemma4\nUNIX_SOCKET=server.sock\n",
            ),
            (
                "provider-4",
                "SNAP_NAME=qwen3\nSNAP_INSTANCE_NAME=qwen3\nOPENAI_UNIX_SOCKET=openai.sock\n",
            ),
            (
                "provider-5",
                "SNAP_NAME=smollm2\nSNAP_INSTANCE_NAME=smollm2\nOPENAI_BASE_URL=http://127.0.0.1:8080/v1\n",
            ),
        ] {
            share(&dir, name, env);
        }

        assert_eq!(
            resolve_speech(&dir).expect("speech resolves without ambiguity"),
            Provider {
                socket: s1.join("myna.sock"),
                snap_name: Some("myna-parakeet".into()),
            }
        );

        let text = resolve_text_shares(&dir);
        assert_eq!(
            text.providers,
            vec![TextProvider {
                socket: s2.join("myna.sock"),
                snap_name: "myna-text".into(),
            }]
        );
        assert!(text.unusable.is_empty());
        assert!(!text.needs_restart());
    }

    #[test]
    fn legacy_speech_snap_without_myna_role_is_excluded_from_text_shares() {
        let dir = tmpdir();
        let _speech = serving(&dir, "provider", "myna-parakeet");
        let s2 = share(
            &dir,
            "provider-2",
            "SNAP_NAME=gemma4\nUNIX_SOCKET=server.sock\n",
        );
        let _llm = UnixListener::bind(s2.join("server.sock")).expect("bind");

        assert_eq!(
            resolve_speech(&dir)
                .expect("legacy speech resolves")
                .snap_name
                .as_deref(),
            Some("myna-parakeet")
        );
        assert!(resolve_text_shares(&dir).providers.is_empty());

        for snap in ["myna-parakeet", "myna-fake-backend"] {
            let d = tmpdir();
            let s = share(
                &d,
                "provider",
                &format!("SNAP_NAME={snap}\nUNIX_SOCKET=custom.sock\n"),
            );
            let _sock = UnixListener::bind(s.join("custom.sock")).expect("bind");
            assert_eq!(
                resolve_speech(&d).expect("speech").snap_name.as_deref(),
                Some(snap)
            );
            assert!(resolve_text_shares(&d).providers.is_empty());
        }
    }

    #[test]
    fn unsupported_myna_role_and_malformed_text_shares_are_handled_safely() {
        let text_only = tmpdir();
        share(
            &text_only,
            "provider",
            "SNAP_NAME=myna-text\nUNIX_SOCKET=../outside.sock\nMYNA_ROLE=text\n",
        );
        assert_eq!(
            not_connected(resolve_speech(&text_only)),
            Unusable::default()
        );
        assert_eq!(resolve_text_shares(&text_only).unusable.malformed, 1);

        let dir = tmpdir();
        let other = share(
            &dir,
            "provider",
            "SNAP_NAME=myna-tts\nUNIX_SOCKET=tts.sock\nMYNA_ROLE=tts\n",
        );
        let _tts = UnixListener::bind(other.join("tts.sock")).expect("bind");
        share(
            &dir,
            "provider-2",
            "SNAP_NAME=myna-text\nUNIX_SOCKET=../outside.sock\nMYNA_ROLE=text\n",
        );
        share(
            &dir,
            "provider-3",
            "SNAP_NAME=myna-text-2\nMYNA_ROLE=text\n",
        );
        share(
            &dir,
            "provider-4",
            "SNAP_NAME=gemma4\nUNIX_SOCKET=/abs/server.sock\n",
        );

        assert_eq!(
            not_connected(resolve_speech(&dir)),
            Unusable::default(),
            "a text share is no speech share, however malformed"
        );
        let text = resolve_text_shares(&dir);
        assert!(text.providers.is_empty());
        assert!(text.unusable.not_serving.is_empty());
        assert_eq!(
            text.unusable.no_unix_socket,
            vec!["myna-text-2".to_string()]
        );
        assert_eq!(text.unusable.malformed, 1);
        assert!(!text.unusable.is_empty());
    }
}
