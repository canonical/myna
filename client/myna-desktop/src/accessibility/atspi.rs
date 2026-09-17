//! The `atspi`-backed [`crate::accessibility::AccessibilityAnnouncer`]
//! implementation: connects to `org.a11y.Bus`, registers an accessible object
//! for the dictation session, and emits AT-SPI `Announcement` events
//! (research.md R1). Headless — no GTK dependency required.
