//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! The contract is `myna_platform::text_input`; this module holds its backends.
//! [`ibus::IbusInjector`] is the default backend, [`extension::ExtensionInjector`]
//! uses an explicitly selected IM extension, and [`mock::MockInjector`] is the
//! hermetic test fixture.

pub use myna_platform::text_input::{
    Activation, FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

pub mod extension;
pub mod ibus;
pub mod lazy;
pub mod mock;
