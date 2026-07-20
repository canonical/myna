//! `ControlTrigger` — activation over a small Unix control socket (the
//! works-today path on GNOME/Wayland for an unsandboxed binary).
//!
//! The GlobalShortcuts portal ([`super::portal`]) is the *packaged* activation
//! (snap/flatpak — it needs an app identity GNOME will only grant a sandboxed or
//! `.desktop`-launched app). For a plain dev binary, `myna-desktop` instead runs
//! as a background **daemon** that listens on a control socket, and a GNOME
//! **custom keyboard shortcut** bound to `myna-desktop --toggle` pokes it. Each
//! poke flips the state: first = `Press` (start), next = `Release` (stop) —
//! toggle-to-talk. No terminal focus, no portal, no app id.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};

use super::{Trigger, TriggerEdge};

/// The default control-socket path (`$XDG_RUNTIME_DIR/myna-desktop.sock`, else
/// `/tmp`).
pub fn default_socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join("myna-desktop.sock")
}

/// A [`Trigger`] fed by pokes on a Unix control socket. Each accepted connection
/// (one per `myna-desktop --toggle`) yields one edge, alternating
/// `Press`/`Release` — toggle-to-talk.
pub struct ControlTrigger {
    listener: UnixListener,
    path: PathBuf,
    pressed: bool,
}

impl ControlTrigger {
    /// Bind the control socket, replacing a stale one left by a previous daemon.
    pub fn bind(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        // A leftover socket file from a crashed daemon would make bind() fail
        // with EADDRINUSE; clear it (best-effort) first.
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        Ok(Self { listener, path, pressed: false })
    }
}

impl Drop for ControlTrigger {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[async_trait]
impl Trigger for ControlTrigger {
    async fn next_edge(&mut self) -> Option<TriggerEdge> {
        // Each `--toggle` connects, (optionally) sends a byte, and disconnects;
        // we treat every poke as a toggle. A listener error ends the trigger.
        let (mut conn, _) = self.listener.accept().await.ok()?;
        let mut buf = [0u8; 16];
        let _ = conn.read(&mut buf).await; // content ignored; presence is the signal
        self.pressed = !self.pressed;
        Some(if self.pressed { TriggerEdge::Press } else { TriggerEdge::Release })
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

    #[tokio::test]
    async fn each_poke_alternates_press_release() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("myna-ctl-test-{}.sock", std::process::id()));
        let mut trigger = ControlTrigger::bind(&path).unwrap();

        // Two pokes → Press then Release.
        send_toggle(&path).await.unwrap();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
        send_toggle(&path).await.unwrap();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Release));
        // Third poke → Press again (next utterance).
        send_toggle(&path).await.unwrap();
        assert_eq!(trigger.next_edge().await, Some(TriggerEdge::Press));
    }

    #[tokio::test]
    async fn toggle_without_daemon_errors_cleanly() {
        let path = std::env::temp_dir().join("myna-ctl-absent.sock");
        let _ = std::fs::remove_file(&path);
        assert!(send_toggle(&path).await.is_err());
    }
}
