//! The one portal shortcut dialog this daemon may have up.
//!
//! GNOME's "Add Keyboard Shortcuts" sheet stays up until the user answers it,
//! whatever happens to the request, the session or the process that asked
//! (measured on 26.04), so every bind that can raise one claims this slot
//! first: `BindShortcut` from a client, and the retry loop's consented
//! re-bind. The daemon publishes the slot as `ShortcutDialog`.

use std::sync::Arc;

use tokio::sync::watch;

/// Shared by everything in the daemon that can raise the sheet.
#[derive(Clone)]
pub struct DialogSlot(Arc<watch::Sender<bool>>);

impl Default for DialogSlot {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }
}

impl DialogSlot {
    /// Take the slot, or `None` while another bind's dialog holds it.
    pub fn claim(&self) -> Option<DialogOpen> {
        let claimed = self
            .0
            .send_if_modified(|open| !std::mem::replace(open, true));
        claimed.then(|| DialogOpen(Arc::clone(&self.0)))
    }

    pub fn is_open(&self) -> bool {
        *self.0.borrow()
    }

    /// Sees every change of [`Self::is_open`].
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.0.subscribe()
    }
}

/// Holds the slot; dropping it, however the bind ends, frees it.
pub struct DialogOpen(Arc<watch::Sender<bool>>);

impl Drop for DialogOpen {
    fn drop(&mut self) {
        self.0.send_replace(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_claim_at_a_time() {
        let slot = DialogSlot::default();
        let open = slot.claim().expect("a free slot");
        assert!(slot.is_open());
        assert!(slot.claim().is_none(), "a second claim got the open slot");
        assert!(slot.is_open(), "a refused claim freed the open slot");
        drop(open);
        assert!(!slot.is_open());
        assert!(slot.claim().is_some(), "a freed slot stayed taken");
    }

    #[test]
    fn clones_share_the_slot() {
        let slot = DialogSlot::default();
        let _open = slot.clone().claim().expect("a free slot");
        assert!(slot.claim().is_none());
    }

    #[tokio::test]
    async fn subscribers_see_it_open_and_close() {
        let slot = DialogSlot::default();
        let mut seen = slot.subscribe();
        let open = slot.claim().expect("a free slot");
        seen.changed().await.expect("open announced");
        assert!(*seen.borrow_and_update());
        let _ = slot.claim();
        assert!(
            !seen.has_changed().unwrap(),
            "a refused claim was announced"
        );
        drop(open);
        seen.changed().await.expect("close announced");
        assert!(!*seen.borrow_and_update());
    }
}
