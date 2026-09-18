//! Re-export of `myna_core::failure` (feature 011-accessible-dictation-ux,
//! US4). The `FailurePresentation` registry lives in `myna-core` (not here)
//! so `myna-cli` (T071) can render the identical presentations on stderr
//! without depending on this crate's D-Bus/IBus/GTK stack - both `myna-cli`
//! and `myna-desktop` already depend on `myna-core`. Kept as a `crate::failure`
//! re-export so existing call sites in this crate don't need to change their
//! import path.
pub use myna_core::failure::*;
