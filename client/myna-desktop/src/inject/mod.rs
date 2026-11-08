//! The text-injection boundary (plan T22, UD129 Text Injection Layer).
//!
//! The contract is `myna_platform::text_input`; this module holds its backends.
//! [`external::ExternalInjector`] uses an installed input-method extension
//! when its socket is present; [`ibus::IbusInjector`] is the fallback and
//! [`mock::MockInjector`] is the hermetic test fixture.

pub use myna_platform::text_input::{
    Activation, FocusEvent, InjectError, Injector, Support, Target, TextInputCapabilities,
};

pub mod external;
pub mod ibus;
pub mod lazy;
pub mod mock;
