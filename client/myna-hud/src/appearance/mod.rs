//! The HUD's [`Appearance`]: the settings portal over what GTK sees, composed
//! for the profile `main` resolved.
//!
//! The pill asks for readings and subscribes to changes; the palette stays
//! in [`crate::accent`].

use std::rc::Rc;

use gtk::prelude::*;
use gtk4 as gtk;
use myna_freedesktop::portal::{AccentFrom, PortalAppearance};
use myna_platform::appearance::Appearance;
use myna_platform::Profile;

pub mod generic;
pub mod probe;

/// The appearance `profile` names, the one the composition root resolved.
/// `accent_widget` is the widget styled `color: @accent_bg_color` (see
/// `style.css`'s `.myna-hud-ribbon`). On GNOME the theme's accent wins: Ubuntu
/// tints a named accent with Yaru's shade, as GNOME Shell does, and the
/// portal reports the untinted one.
pub fn for_profile(profile: Profile, accent_widget: &impl IsA<gtk::Widget>) -> Rc<dyn Appearance> {
    let accent = match profile {
        Profile::Gnome => AccentFrom::Fallback,
        Profile::Xfce | Profile::Kde | Profile::Generic => AccentFrom::Portal,
    };
    PortalAppearance::session(Rc::new(generic::GtkAppearance::new(accent_widget)), accent)
}
