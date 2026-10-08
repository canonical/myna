//! The handle a watch returns: dropping it stops the notifications.

/// Owns one watch. Dropping it runs the backend's cancel, once.
#[must_use = "dropping a Subscription ends the watch"]
pub struct Subscription(Option<Box<dyn FnOnce()>>);

impl Subscription {
    pub fn new(cancel: impl FnOnce() + 'static) -> Self {
        Self(Some(Box::new(cancel)))
    }

    /// A watch that lasts as long as the process, for backends that cannot
    /// disconnect.
    pub fn forever() -> Self {
        Self(None)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel();
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Subscription"
        } else {
            "Subscription(forever)"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;

    #[test]
    fn dropping_cancels_once() {
        let cancelled = Rc::new(Cell::new(0));
        let subscription = Subscription::new({
            let cancelled = Rc::clone(&cancelled);
            move || cancelled.set(cancelled.get() + 1)
        });
        assert_eq!(cancelled.get(), 0);
        assert_eq!(format!("{subscription:?}"), "Subscription");
        drop(subscription);
        assert_eq!(cancelled.get(), 1);
        assert_eq!(
            format!("{:?}", Subscription::forever()),
            "Subscription(forever)"
        );
    }
}
