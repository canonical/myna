//! Backends over freedesktop.org services that more than one Myna process
//! uses, on gio, and the stand-ins their tests run against.

pub mod bus;

#[cfg(feature = "testing")]
pub mod testing;
