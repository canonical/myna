//! `ControlTrigger` — activation by pokes, each one a toggle: first = `Press`
//! (start), next = `Release` (stop).
//!
//! Two things poke it. The desktop custom shortcut calls the daemon's
//! `com.canonical.Myna.Dictation.Toggle` over D-Bus ([`Poke`]), and
//! `myna-desktop --toggle` (`/snap/bin/myna.toggle`) connects to a Unix control
//! socket ([`listen`]), which shortcuts older Myna Settings wrote still run.
//! Both feed one channel, so they share one press/release parity, and a held
//! key's repeats, from either, toggle nothing ([`REPEAT_QUIET`]).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::time::Instant;

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
/// A held key repeats: X11 makes xfsettingsd run the shortcut's command again
/// after the repeat delay (500 ms on Xubuntu, 660 ms in a bare X server), then
/// every 50 ms, and GNOME every 30 ms. The first repeat was measured reaching
/// the daemon 574 ms after the press. A poke within this of the one before may
/// be a repeat; the margin covers a slower command start.
pub const REPEAT_QUIET: Duration = Duration::from_millis(800);
/// A poke within [`REPEAT_QUIET`] is a repeat when another follows it within
/// this, as repeats do and a second press does not. Consecutive repeats were
/// measured up to 93 ms apart.
pub const REPEAT_FOLLOW: Duration = Duration::from_millis(200);
/// First socket bind retry delay; doubles per failure up to [`RETRY_MAX`].
const RETRY_START: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);

/// A [`Trigger`] that yields one edge per press, alternating
/// `Press`/`Release`, and none for a held key's repeats.
pub struct ControlTrigger {
    poke: Poke,
    rx: mpsc::Receiver<Arrival>,
    pressed: bool,
    /// Within `quiet` of the poke before it: a press unless another follows
    /// within [`REPEAT_FOLLOW`]. Kept here, not in a future, so a `next_edge`
    /// dropped while it waits loses nothing.
    candidate: Option<Arrival>,
    /// Read while deciding a candidate, and not one of its repeats.
    later: Option<Arrival>,
    /// A repeat stream is under way: pokes within `quiet` of the one before
    /// are dropped until the pokes go quiet.
    streaming: bool,
    quiet: Duration,
}

/// One poke, stamped as it arrived, since the controller may not read it for
/// a second.
#[derive(Clone, Copy, Debug)]
struct Arrival {
    at: Instant,
    /// Since the poke before, including one the full backlog dropped.
    gap: Option<Duration>,
}

/// Pokes a [`ControlTrigger`]: what the served `Toggle` method holds.
#[derive(Clone)]
pub struct Poke {
    tx: mpsc::Sender<Arrival>,
    last: Arc<Mutex<Option<Instant>>>,
}

impl Poke {
    /// Never blocks: a full backlog drops the poke.
    pub fn poke(&self) {
        let at = Instant::now();
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let gap = last.map(|before| at.duration_since(before));
        *last = Some(at);
        let _ = self.tx.try_send(Arrival { at, gap });
    }
}

impl ControlTrigger {
    pub fn new() -> Self {
        Self::with_quiet(REPEAT_QUIET)
    }

    fn with_quiet(quiet: Duration) -> Self {
        let (tx, rx) = mpsc::channel(BACKLOG);
        Self {
            poke: Poke {
                tx,
                last: Arc::new(Mutex::new(None)),
            },
            rx,
            pressed: false,
            candidate: None,
            later: None,
            streaming: false,
            quiet,
        }
    }

    pub fn poke(&self) -> Poke {
        self.poke.clone()
    }

    fn toggle(&mut self) -> TriggerEdge {
        self.pressed = !self.pressed;
        if self.pressed {
            TriggerEdge::Press
        } else {
            TriggerEdge::Release
        }
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
        loop {
            if let Some(candidate) = self.candidate {
                let next = match self.later.take() {
                    Some(next) => Some(next),
                    None => {
                        let deadline = candidate.at + REPEAT_FOLLOW;
                        tokio::time::timeout_at(deadline, self.rx.recv())
                            .await
                            .ok()
                            .flatten()
                    }
                };
                self.candidate = None;
                match next {
                    Some(next) if next.gap.is_some_and(|gap| gap < REPEAT_FOLLOW) => {
                        self.streaming = true;
                        continue;
                    }
                    next => self.later = next,
                }
                return Some(self.toggle());
            }
            let arrival = match self.later.take() {
                Some(arrival) => arrival,
                None => self.rx.recv().await?,
            };
            if !arrival.gap.is_some_and(|gap| gap < self.quiet) {
                self.streaming = false;
                return Some(self.toggle());
            }
            if !self.streaming {
                self.candidate = Some(arrival);
            }
        }
    }

    /// Drop queued pokes *without* flipping `pressed` — called by the
    /// controller at the end of an utterance to swallow hotkey spam that
    /// arrived during Finalizing, so the next real tap still delivers `Press`.
    async fn discard_pending(&mut self) {
        self.candidate = None;
        self.later = None;
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

    /// A trigger on a fresh socket. These tests poke faster than a person,
    /// so bursts are not coalesced unless `quiet` says so.
    fn listening_with(tag: &str, quiet: Duration) -> (ControlTrigger, PathBuf) {
        let path = std::env::temp_dir().join(format!("myna-ctl-{tag}-{}.sock", std::process::id()));
        let trigger = ControlTrigger::with_quiet(quiet);
        let _ = std::fs::remove_file(&path);
        tokio::spawn(listen(path.clone(), trigger.poke()));
        (trigger, path)
    }

    fn listening(tag: &str) -> (ControlTrigger, PathBuf) {
        listening_with(tag, Duration::ZERO)
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
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!has_edge(&mut trigger).await, "spam survived the drain");
    }

    #[tokio::test]
    async fn discard_pending_is_a_noop_when_empty() {
        let mut trigger = ControlTrigger::new();
        tokio::time::timeout(Duration::from_millis(200), trigger.discard_pending())
            .await
            .expect("discard_pending must not block on an empty backlog");
    }

    /// Whether an edge is waiting, without waiting for one.
    async fn has_edge(trigger: &mut ControlTrigger) -> bool {
        tokio::time::timeout(Duration::from_millis(1), trigger.next_edge())
            .await
            .is_ok()
    }

    // A held shortcut on X11: xfsettingsd runs its command again on every key
    // repeat, after the repeat delay (500 ms on Xubuntu) and then every 50 ms.
    #[tokio::test(start_paused = true)]
    async fn a_held_keys_repeats_are_one_toggle() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let mut last = Instant::now();
        for _ in 0..20 {
            poke.poke();
            last = Instant::now();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert!(!has_edge(&mut trigger).await, "a repeat toggled again");

        // Quiet as long as a repeat may lag: a press, taken at once.
        tokio::time::sleep_until(last + REPEAT_QUIET).await;
        poke.poke();
        let now = tokio::time::timeout(Duration::from_millis(1), trigger.next_edge()).await;
        assert_eq!(
            now,
            Ok(Some(TriggerEdge::Release)),
            "the press was not taken at once"
        );
    }

    // A second press soon after the first is one, once no repeat follows it.
    #[tokio::test(start_paused = true)]
    async fn a_quick_second_press_still_toggles() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        tokio::time::sleep(Duration::from_millis(300)).await;
        poke.poke();
        let pressed = Instant::now();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
        assert!(
            pressed.elapsed() >= REPEAT_FOLLOW,
            "decided before a repeat could follow"
        );
    }

    // That second press held down: it toggles, its repeats do not.
    #[tokio::test(start_paused = true)]
    async fn a_quick_second_press_held_toggles_once() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        tokio::time::sleep(Duration::from_millis(300)).await;
        poke.poke();
        tokio::time::sleep(Duration::from_millis(550)).await;
        for _ in 0..10 {
            poke.poke();
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
        assert!(!has_edge(&mut trigger).await, "a repeat toggled");
    }

    // The controller reads the trigger in a select, so a read dropped while
    // it waits to see whether a repeat follows must not lose the press.
    #[tokio::test(start_paused = true)]
    async fn a_read_dropped_mid_decision_keeps_the_press() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        tokio::time::sleep(Duration::from_millis(300)).await;
        poke.poke();
        let early = tokio::time::timeout(Duration::from_millis(10), trigger.next_edge()).await;
        assert!(early.is_err(), "decided before a repeat could follow");
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
    }

    // A press read while deciding the one before it is not lost either.
    #[tokio::test(start_paused = true)]
    async fn a_press_after_the_window_is_kept() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        tokio::time::sleep(Duration::from_millis(300)).await;
        poke.poke();
        tokio::time::sleep(REPEAT_FOLLOW).await;
        poke.poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
    }

    #[tokio::test(start_paused = true)]
    async fn discard_pending_drops_a_press_being_decided() {
        let mut trigger = ControlTrigger::new();
        let poke = trigger.poke();

        poke.poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        tokio::time::sleep(Duration::from_millis(300)).await;
        poke.poke();
        let _ = tokio::time::timeout(Duration::from_millis(10), trigger.next_edge()).await;
        trigger.discard_pending().await;
        tokio::time::sleep(REPEAT_FOLLOW).await;
        assert!(!has_edge(&mut trigger).await, "the discarded press toggled");
        tokio::time::sleep(REPEAT_QUIET).await;
        poke.poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
    }

    // Both channels make one stream: socket pokes repeating a D-Bus one.
    #[tokio::test]
    async fn the_socket_and_a_poke_share_one_stream() {
        let (mut trigger, path) = listening_with("stream", REPEAT_QUIET);

        trigger.poke().poke();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        tokio::time::sleep(Duration::from_millis(300)).await;
        toggle(&path).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        toggle(&path).await;
        tokio::time::sleep(REPEAT_FOLLOW + Duration::from_millis(50)).await;
        assert!(
            !has_edge(&mut trigger).await,
            "the socket's repeats toggled"
        );
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
