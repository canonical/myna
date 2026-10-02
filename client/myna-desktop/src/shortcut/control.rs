//! `ControlTrigger` — activation by pokes, each one a toggle: first = `Press`
//! (start), next = `Release` (stop).
//!
//! Two things poke it. The desktop custom shortcut calls the daemon's
//! `com.canonical.Myna.Dictation.Toggle` over D-Bus ([`Poke`]), and
//! `myna-desktop --toggle` (`/snap/bin/myna.toggle`) connects to a Unix control
//! socket ([`listen`]), which shortcuts older Myna Settings wrote still run.
//! Both feed one channel, so they share one press/release parity.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use super::{Trigger, TriggerEdge};

/// The default control-socket path (`$XDG_RUNTIME_DIR/myna-desktop.sock`,
/// else `/tmp`). Under snap confinement `$XDG_RUNTIME_DIR` is already
/// snap-scoped (`/run/user/<uid>/snap.<instance>`) and writable, so the same
/// filename works unchanged — a plain `$XDG_RUNTIME_DIR/myna-desktop.sock`
/// would only break for a *confined* process with an unscoped runtime dir,
/// which snapd never produces.
pub fn default_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("myna-desktop.sock")
}

/// Pokes queued beyond this are hotkey spam and dropped.
const BACKLOG: usize = 8;
/// First socket bind retry delay; doubles per failure up to [`RETRY_MAX`].
const RETRY_START: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// A [`Trigger`] that yields one edge per poke, alternating `Press`/`Release`.
pub struct ControlTrigger {
    tx: mpsc::Sender<()>,
    rx: mpsc::Receiver<()>,
    pressed: bool,
}

/// Pokes a [`ControlTrigger`]: what the served `Toggle` method holds.
#[derive(Clone)]
pub struct Poke(mpsc::Sender<()>);

impl Poke {
    /// Never blocks: a full backlog drops the poke.
    pub fn poke(&self) {
        let _ = self.0.try_send(());
    }
}

impl ControlTrigger {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(BACKLOG);
        Self {
            tx,
            rx,
            pressed: false,
        }
    }

    pub fn poke(&self) -> Poke {
        Poke(self.tx.clone())
    }
}

impl Default for ControlTrigger {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Trigger for ControlTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        self.rx.recv().await?;
        self.pressed = !self.pressed;
        Some(if self.pressed {
            TriggerEdge::Press
        } else {
            TriggerEdge::Release
        })
    }

    /// Drop queued pokes *without* flipping `pressed` — called by the
    /// controller at the end of an utterance to swallow hotkey spam that
    /// arrived during Finalizing, so the next real tap still delivers `Press`.
    async fn discard_pending(&mut self) {
        while self.rx.try_recv().is_ok() {}
    }

    /// Force `pressed` back to `false` when an utterance ended for a reason
    /// other than a matching `Release` (e.g. focus loss), so the next poke
    /// still delivers `Press` (see the `Trigger::resync` doc comment).
    async fn resync(&mut self) {
        self.pressed = false;
    }
}

/// Bind the control socket at `path` and turn every connection into a poke.
/// The bind is retried with
/// backoff: `$XDG_RUNTIME_DIR` is created by pam_systemd, so a daemon that
/// starts early can find it not there yet. The first failure is logged, the
/// repeats only under `MYNA_DEBUG`.
pub async fn listen(path: PathBuf, poke: Poke) {
    let mut delay = RETRY_START;
    let listener = loop {
        match bind(&path) {
            Ok(listener) => break listener,
            Err(e) if delay == RETRY_START => myna_core::info_log!(
                "trigger",
                "cannot bind control socket {} ({e}); retrying, backing off to {RETRY_MAX:?}",
                path.display()
            ),
            Err(e) => myna_core::dbg_log!("trigger", "still cannot bind control socket ({e})"),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    };
    let _socket = Socket(path);
    while let Ok((mut conn, _)) = listener.accept().await {
        let mut buf = [0u8; 16];
        let _ = conn.read(&mut buf).await; // content ignored; presence is the signal
        poke.poke();
    }
}

/// Bind, replacing a stale socket a crashed daemon left behind (which would
/// otherwise fail with EADDRINUSE).
fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path)
}

/// Removes the socket file when the listener goes.
struct Socket(PathBuf);

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Client side of `--toggle`: connect to the daemon's control socket and poke
/// it. Returns an error if no daemon is listening.
pub async fn send_toggle(path: impl AsRef<Path>) -> std::io::Result<()> {
    let mut conn = UnixStream::connect(path.as_ref()).await?;
    use tokio::io::AsyncWriteExt;
    conn.write_all(b"toggle").await?;
    conn.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listening(tag: &str) -> (ControlTrigger, PathBuf) {
        let path = std::env::temp_dir().join(format!("myna-ctl-{tag}-{}.sock", std::process::id()));
        let trigger = ControlTrigger::new();
        let _ = std::fs::remove_file(&path);
        tokio::spawn(listen(path.clone(), trigger.poke()));
        (trigger, path)
    }

    async fn toggle(path: &Path) {
        for _ in 0..100 {
            if send_toggle(path).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the control socket never came up");
    }

    #[tokio::test]
    async fn each_poke_alternates_press_release() {
        let (mut trigger, path) = listening("alternate");

        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
    }

    // A socket poke and a D-Bus poke are the same toggle.
    #[tokio::test]
    async fn the_socket_and_a_poke_share_one_parity() {
        let (mut trigger, path) = listening("shared");

        trigger.poke().poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
    }

    #[tokio::test]
    async fn toggle_without_daemon_errors_cleanly() {
        let path = std::env::temp_dir().join("myna-ctl-absent.sock");
        let _ = std::fs::remove_file(&path);
        assert!(send_toggle(&path).await.is_err());
    }

    // Hotkey-spam regression: pokes that arrive while the controller has the
    // trigger paused (Finalizing) queue up. `discard_pending()` drops them
    // WITHOUT flipping `pressed`, so the next real poke still lands as the
    // expected edge (no phantom Recording→Finalizing cycles, no "first tap
    // does nothing" desync).
    #[tokio::test]
    async fn discard_pending_drains_backlog_without_flipping_parity() {
        let (mut trigger, path) = listening("drain");

        // Start dictation, then "end" it — trigger is now at pressed=false.
        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));

        // The user spamming the hotkey during Finalizing.
        for _ in 0..5 {
            toggle(&path).await;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;

        trigger.discard_pending().await;

        toggle(&path).await;
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
    }

    #[tokio::test]
    async fn discard_pending_is_a_noop_when_empty() {
        let mut trigger = ControlTrigger::new();
        tokio::time::timeout(Duration::from_millis(200), trigger.discard_pending())
            .await
            .expect("discard_pending must not block on an empty backlog");
    }

    // `$XDG_RUNTIME_DIR` missing at login: the bind waits for it.
    #[tokio::test(start_paused = true)]
    async fn the_bind_is_retried_until_the_directory_exists() {
        let dir = std::env::temp_dir().join(format!("myna-ctl-late-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("myna-desktop.sock");
        let mut trigger = ControlTrigger::new();
        tokio::spawn(listen(path.clone(), trigger.poke()));

        tokio::time::sleep(Duration::from_secs(2)).await;
        std::fs::create_dir_all(&dir).unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(path.exists(), "bound once the directory appeared");

        send_toggle(&path).await.unwrap();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
