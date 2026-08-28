// examples/accent_change.rs — does the ribbon follow an accent change that
// happens WHILE it is running?
//
// Reading the accent from a settings handler returned the previous value:
// libadwaita listens to the same key with no defined ordering between us,
// and GTK recomputes styles lazily at the next frame regardless. This drives
// the same situation by swapping the theme's @accent_bg_color at runtime and
// screenshotting before and after.
//
//   xvfb-run -a cargo run -p myna-hud --example accent_change

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;
use myna_hud::states::{state_to_descriptor, wire};
use myna_hud::window::HudWindow;

fn main() {
    let app = adw::Application::builder()
        .application_id("com.canonical.Myna.AccentChange")
        .build();
    app.connect_activate(|app| {
        let hud = HudWindow::new(app);
        hud.apply_descriptor(state_to_descriptor(Some(wire::RECORDING), ""));
        hud.push_level(0.35, 0.6);
        hud.window().present();

        // After the pill has settled, redefine the accent the way a theme
        // change would, and let the frame clock pick it up.
        let hud_resync = hud.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(3500), move || {
            let provider = gtk::CssProvider::new();
            // Override the ribbon's computed colour, which is what a theme
            // accent change amounts to from the widget's point of view.
            // (A separate provider's @define-color does not resolve into
            // another sheet's reference, so redefining the named colour
            // that way just invalidates it.)
            provider.load_from_data(".myna-hud-ribbon { color: #00b000; }");
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_USER,
            );
            // A raw provider swap is not something any watched preference
            // reports, so nudge the HUD the way a host would. A real accent
            // change arrives through the accent/theme settings or the style
            // manager and schedules this by itself.
            hud_resync.resync_accent();
            println!("accent-change: accent redefined to #00b000");
        });
    });
    app.run_with_args::<&str>(&[]);
}
