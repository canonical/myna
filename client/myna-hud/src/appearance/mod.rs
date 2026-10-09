//! The HUD's [`Appearance`] backends, one per desktop profile, and the one
//! place that picks them.
//!
//! The pill asks a backend for readings and subscribes to changes; the
//! palette and animation policy stay in [`crate::accent`] and
//! [`crate::motion`].

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use myna_platform::appearance::Appearance;
use myna_platform::{Profile, SessionEnv};

pub mod generic;
pub mod gnome;
pub mod probe;

/// The backend `profile` names. `accent_widget` is the widget styled `color:
/// @accent_bg_color` (see `style.css`'s `.myna-hud-ribbon`).
pub fn for_profile(profile: Profile, accent_widget: &impl IsA<gtk::Widget>) -> Rc<dyn Appearance> {
    match profile {
        Profile::Gnome => Rc::new(gnome::GnomeAppearance::new(accent_widget)),
        Profile::Xfce | Profile::Generic => Rc::new(generic::GtkAppearance::new(accent_widget)),
    }
}

/// The backend of this process's session. A `MYNA_PLATFORM` naming no profile
/// is reported and read as the generic one.
///
/// The pill calls this itself; the HUD's composition root will hand it a
/// profile resolved once for every module.
pub fn for_process(accent_widget: &impl IsA<gtk::Widget>) -> Rc<dyn Appearance> {
    let profile = Profile::select(&SessionEnv::from_process()).unwrap_or_else(|error| {
        eprintln!("myna-hud: {error}");
        Profile::Generic
    });
    for_profile(profile, accent_widget)
}
