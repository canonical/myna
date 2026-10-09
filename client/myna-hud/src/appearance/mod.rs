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

pub mod gnome;
pub mod probe;

/// The backend of this process's desktop.
pub fn for_process(accent_widget: &impl IsA<gtk::Widget>) -> Rc<dyn Appearance> {
    Rc::new(gnome::GnomeAppearance::new(accent_widget))
}
