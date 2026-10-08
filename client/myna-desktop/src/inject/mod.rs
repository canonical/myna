//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! The contract is `myna_platform::text_input`; this module holds its backends.
//! [`ibus::IbusInjector`] is the shipped implementor; [`mock::MockInjector`] is
//! the hermetic test fixture.

pub use myna_platform::text_input::{
    FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

pub mod ibus;
pub mod lazy;
pub mod mock;
