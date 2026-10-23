// examples/platform_probe.rs — prints what `platform.rs` sees on THIS
// stack (feature 004, T122). A diagnostic, not a test: the runtime matrix
// spans GTK 4.14/4.18/4.22 and libadwaita 1.5/1.7/1.9, and this is how a
// developer confirms which sources a given machine actually offers.
//
// Run with:  xvfb-run -a cargo run -p myna-hud --example platform_probe
// `--watch` keeps it running and prints the readings at every change.

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;

use myna_hud::accent::palette_for;
use myna_hud::appearance::probe as platform;
use myna_hud::platform::Platform;

fn main() {
    let app = adw::Application::builder()
        .application_id("com.canonical.Myna.HudPlatformProbe")
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
            "gtk-enable-animations: {:?}",
            platform::probe_gtk_enable_animations()
        );
        println!(
            "resolved             : {}",
            myna_hud::appearance::for_profile(Platform::current().profile, &gtk::Label::new(None))
                .read()
                .reduced_motion
        );
        println!("--- accent (R18/R26) ---");
        // A widget styled the way `style.css` styles the ribbon, so the
        // CSS path is exercised exactly as production does it.
        let provider = gtk::CssProvider::new();
        provider.load_from_string(".myna-accent-probe { color: @accent_bg_color; }");
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
        let appearance = myna_hud::appearance::for_profile(Platform::current().profile, &probe);
        let palette = palette_for(appearance.read().accent);
        println!("ribbon palette       : {palette:?}");

        if !std::env::args().any(|arg| arg == "--watch") {
            app.quit();
            return;
        }
        let hold = app.hold();
        let subscription = appearance.watch(Box::new({
            let appearance = appearance.clone();
            move |freshness| println!("changed ({freshness:?}): {:?}", appearance.read())
        }));
        // Lives as long as the application.
        std::mem::forget((hold, subscription));
    });

    app.run_with_args::<&str>(&[]);
}
