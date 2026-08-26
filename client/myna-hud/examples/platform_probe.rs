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
            "accent user value    : {:?}   (None = never written -> Ubuntu orange)",
            platform::probe_accent_user_value()
        );
        println!(
            "adw resolved rgba    : {:?}   (None = libadwaita < 1.7)",
            platform::probe_platform_accent()
        );
        let palette = platform::probe_accent_palette();
        println!("ribbon palette       : {palette:?}");

        app.quit();
    });

    app.run();
}
