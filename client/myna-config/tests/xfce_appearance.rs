//! Xfce's theme is read from xfconf: libadwaita rewrites `gtk-theme-name` to
//! its own as it starts, and then no longer notices a change of the
//! desktop's. The stand-in xfconfd stands for xfsettingsd's channel.

mod common;

use std::cell::Cell;
use std::rc::Rc;

use common::*;
use gio::prelude::*;
use myna_config::platform::appearance::GtkAppearance;
use myna_config::platform::xfce::xfconf::Xfconf;
use myna_platform::appearance::Appearance;

const THEME: &str = "/Net/ThemeName";

fn appearance(connection: &gio::DBusConnection) -> GtkAppearance {
    GtkAppearance::with_xfconf(Xfconf::with_connection(connection.clone(), "xsettings"))
}

/// GTK belongs to one thread, so one test drives both halves.
#[test]
fn the_desktops_theme_comes_from_xfconf() {
    if std::env::var_os("MYNA_CONFIG_GTK_TESTS").is_none() {
        eprintln!("skipped: set MYNA_CONFIG_GTK_TESTS=1 under Xvfb");
        return;
    }
    gtk4::init().expect("display");
    the_themes_name_in_xfconf_says_dark_and_high_contrast();
    a_change_of_theme_is_heard_until_the_watch_is_dropped();
}

fn the_themes_name_in_xfconf_says_dark_and_high_contrast() {
    on_own_context(|| {
        let (connection, _bus, _thread) = connect();
        let desktop = Xfconf::with_connection(connection.clone(), "xsettings");
        let appearance = appearance(&connection);
        for (theme, dark, contrast) in [
            ("Greybird", false, false),
            ("Greybird-dark", true, false),
            ("Adwaita-dark", true, false),
            ("HighContrast", false, true),
        ] {
            desktop.set(THEME, &theme.to_variant()).unwrap();
            let readings = appearance.read();
            assert_eq!(readings.prefers_dark, dark, "{theme}");
            assert_eq!(readings.high_contrast, contrast, "{theme}");
        }
    });
}

fn a_change_of_theme_is_heard_until_the_watch_is_dropped() {
    on_own_context(|| {
        let (connection, _bus, _thread) = connect();
        let desktop = Xfconf::with_connection(connection.clone(), "xsettings");
        let heard = Rc::new(Cell::new(0));
        let subscription = appearance(&connection).watch(Box::new({
            let heard = heard.clone();
            move |_| heard.set(heard.get() + 1)
        }));
        desktop.set(THEME, &"Greybird-dark".to_variant()).unwrap();
        settle();
        assert!(heard.get() > 0, "the theme changed unheard");
        desktop
            .set("/Net/IconThemeName", &"Tango".to_variant())
            .unwrap();
        settle();
        let before = heard.get();
        desktop
            .set("/Net/IconThemeName", &"Adwaita".to_variant())
            .unwrap();
        settle();
        assert_eq!(heard.get(), before, "another property was heard");
        drop(subscription);
        desktop.set(THEME, &"Greybird".to_variant()).unwrap();
        settle();
        assert_eq!(heard.get(), before, "heard after the drop");
    });
}
