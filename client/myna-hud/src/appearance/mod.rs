//! The HUD's [`Appearance`] backends, one per desktop profile, and the one
//! place that picks them from the profile `main` resolved.
//!
//! The pill asks a backend for readings and subscribes to changes; the
//! palette and animation policy stay in [`crate::accent`] and
//! [`crate::motion`].

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use myna_platform::appearance::Appearance;
use myna_platform::Profile;

pub mod generic;
pub mod gnome;
pub mod probe;

/// The backend `profile` names, the one the composition root resolved. `accent_widget` is the widget styled `color:
/// @accent_bg_color` (see `style.css`'s `.myna-hud-ribbon`).
pub fn for_profile(profile: Profile, accent_widget: &impl IsA<gtk::Widget>) -> Rc<dyn Appearance> {
    match profile {
        Profile::Gnome => Rc::new(gnome::GnomeAppearance::new(accent_widget)),
        Profile::Xfce | Profile::Generic => Rc::new(generic::GtkAppearance::new(accent_widget)),
    }
}
