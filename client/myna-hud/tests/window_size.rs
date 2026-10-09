//! The overlay window's default size never undercuts what its content needs,
//! at any text size. A default smaller than the content's minimum makes GTK
//! allocate too little and log "Gtk-CRITICAL: Allocation height too small"
//! (a font with a taller line than the one the floor was measured with, seen
//! on Xubuntu in CI). Gated like the other GTK suites (`ui-check`).

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_hud::states::state_to_descriptor;
use myna_hud::window::HudWindow;
use myna_platform::Profile;

#[test]
fn the_default_size_covers_the_content_at_any_text_size() {
    if std::env::var_os("MYNA_HUD_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_HUD_GTK_TESTS=1 under Xvfb");
        return;
    }
    std::env::set_var("GSETTINGS_BACKEND", "memory");
    gtk::test_synced(|| {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let app = adw::Application::builder()
            .application_id("com.canonical.Myna.HudSizeTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).expect("register");
        // The label's size is fixed in the app's CSS and the test machine has
        // one font, so a user stylesheet (which outranks it) stands in for a
        // font with a taller line.
        let user = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &user,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        let hud = HudWindow::new(&app, Profile::Generic);
        for points in [11, 14, 20, 30] {
            user.load_from_string(&format!(".myna-hud-label {{ font-size: {points}pt; }}"));
            for (state, message) in [
                ("recording", ""),
                ("notice", "Focus lost"),
                ("error", "Password field skipped (focused field is secure)"),
            ] {
                hud.apply_descriptor(state_to_descriptor(Some(state), message));
                let window = hud.window();
                let (width, height) = window.default_size();
                let (minimum, ..) = window.measure(gtk::Orientation::Vertical, width);
                assert!(
                    height >= minimum,
                    "{points}pt, {state}: default {width}x{height} under the content's {minimum}"
                );
            }
        }
    });
}
