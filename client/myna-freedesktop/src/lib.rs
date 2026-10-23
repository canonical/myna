//! Backends over freedesktop.org services that more than one Myna process
//! uses, on gio: the settings portal's appearance, and bounded calls.

pub mod bus;
pub mod portal;

#[cfg(feature = "testing")]
pub mod testing;
