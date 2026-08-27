// Which widget paints the halo outside the pill? Applies MYNA_EXTRA_CSS as
// a USER-priority provider on top of the real HUD styling.
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use myna_hud::states::{state_to_descriptor, wire};
use myna_hud::window::HudWindow;

fn main() {
    let app = adw::Application::builder()
        .application_id("org.myna.ShadowProbe")
        .build();
    app.connect_activate(|app| {
        let hud = HudWindow::new(app);
        if let Ok(extra) = std::env::var("MYNA_EXTRA_CSS") {
            if !extra.is_empty() {
                let p = gtk::CssProvider::new();
                p.load_from_data(&extra);
                gtk::style_context_add_provider_for_display(
                    &gtk::gdk::Display::default().unwrap(),
                    &p,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
            }
        }
        hud.apply_descriptor(state_to_descriptor(Some(wire::RECORDING), ""));
        hud.push_level(0.35, 0.6);
        hud.window().present();
    });
    app.run_with_args::<&str>(&[]);
}
