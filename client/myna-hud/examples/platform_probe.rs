// examples/platform_probe.rs — prints what `platform.rs` sees on THIS
// stack (feature 004, T122). A diagnostic, not a test: the runtime matrix
// spans GTK 4.14/4.18/4.22 and libadwaita 1.5/1.7/1.9, and this is how a
// developer confirms which sources a given machine actually offers.
//
// Run with:  xvfb-run -a cargo run -p myna-hud --example platform_probe

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use myna_hud::platform;

fn main() {
    let app = adw::Application::builder()
        .application_id("org.myna.HudPlatformProbe")
        .build();

    app.connect_activate(|app| {
        println!(
            "gtk runtime          : {}.{}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            gtk::micro_version()
        );
        println!(
            "adw runtime          : {}.{}.{}",
            adw::major_version(),
            adw::minor_version(),
            adw::micro_version()
        );
        println!("--- reduced motion (E2b) ---");
        println!(
            "GtkSettings property : {:?}   (None = GTK < 4.22, falls back)",
            platform::probe_gtk_reduced_motion()
        );
        println!(
            "enable-animations    : {:?}   (raw, not inverted)",
            platform::probe_enable_animations()
        );
        println!(
            "resolved             : {}",
            platform::probe_reduced_motion()
        );
        println!("--- accent (R18/R26) ---");
        println!(
            "accent setting value : {:?}   (RESOLVED, not user value)",
            platform::probe_accent_value()
        );
        println!(
            "adw resolved rgba    : {:?}   (None = libadwaita < 1.7)",
            platform::probe_platform_accent()
        );
        // A widget styled the way `style.css` styles the ribbon, so the
        // CSS path is exercised exactly as production does it.
        let provider = gtk::CssProvider::new();
        provider.load_from_data(".myna-accent-probe { color: @accent_bg_color; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let probe = gtk::Label::new(None);
        probe.add_css_class("myna-accent-probe");
        let window = gtk::ApplicationWindow::builder().application(app).build();
        window.set_child(Some(&probe));
        window.present();
        println!(
            "theme @accent_bg_color: {:?}   (the primary source)",
            platform::probe_css_accent(&probe)
        );
        let palette = platform::probe_accent_palette(Some(&probe));
        println!("ribbon palette       : {palette:?}");

        app.quit();
    });

    app.run();
}
