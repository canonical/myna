//! The pill's icons resolve under any icon theme: a confined HUD may not see
//! the theme the desktop names (breeze on Plasma), and GTK then falls back to
//! hicolor alone. Gated like the other GTK suites (`ui-check`).

use gtk::prelude::*;
use gtk4 as gtk;
use libadwaita as adw;
use myna_hud::hud_logic::icon_for_severity;
use myna_hud::states::Severity;
use myna_hud::window::HudWindow;
use myna_platform::Profile;

#[test]
fn the_pill_icons_resolve_without_the_desktop_theme() {
    if std::env::var_os("MYNA_HUD_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_HUD_GTK_TESTS=1 under Xvfb");
        return;
    }
    std::env::set_var("GSETTINGS_BACKEND", "memory");
    gtk::test_synced(|| {
        gtk::init().expect("display");
        adw::init().expect("libadwaita");
        let app = adw::Application::builder()
            .application_id("com.canonical.Myna.HudIconTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).expect("register");
        let _hud = HudWindow::new(&app, Profile::Generic);
        let display = gtk::gdk::Display::default().expect("display");
        gtk::Settings::for_display(&display).set_gtk_icon_theme_name(Some("no-such-theme"));
        let theme = gtk::IconTheme::for_display(&display);
        theme.set_search_path(&[]);
        for severity in [None, Some(Severity::Recoverable), Some(Severity::Critical)] {
            let name = icon_for_severity(severity);
            let icon = theme.lookup_icon(
                name,
                &[],
                24,
                1,
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            );
            let uri = icon.file().map(|file| file.uri().to_string());
            assert!(
                uri.as_deref()
                    .is_some_and(|uri| uri.starts_with("resource:///com/canonical/Myna/Hud/")),
                "{name} resolved to {uri:?}"
            );
        }
    });
}
