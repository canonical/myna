// Can the accent be read straight from CSS, instead of from a name table?
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

fn probe(window: &gtk::ApplicationWindow, css: &str, label: &str) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(".myna-accent-probe {{ color: {css}; }}"));
    gtk::style_context_add_provider_for_display(
        &gtk::gdk::Display::default().unwrap(),
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let w = gtk::Label::new(None);
    w.add_css_class("myna-accent-probe");
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.append(&w);
    window.set_child(Some(&b));
    let c = w.color();
    println!(
        "  {label:<24} -> rgb({:.0},{:.0},{:.0}) a={:.2}   #{:02x}{:02x}{:02x}",
        c.red() * 255.0,
        c.green() * 255.0,
        c.blue() * 255.0,
        c.alpha(),
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8
    );
}

fn main() {
    let app = adw::Application::builder()
        .application_id("com.canonical.Myna.AccentCss")
        .build();
    app.connect_activate(|app| {
        let win = gtk::ApplicationWindow::builder().application(app).build();
        win.present();

        println!(
            "gtk {}.{} / adw {}.{}",
            gtk::major_version(),
            gtk::minor_version(),
            adw::major_version(),
            adw::minor_version()
        );
        println!("-- CSS probes --");
        probe(&win, "@accent_bg_color", "@accent_bg_color");
        probe(&win, "@accent_color", "@accent_color");
        probe(&win, "var(--accent-bg-color)", "var(--accent-bg-color)");
        probe(&win, "var(--accent-color)", "var(--accent-color)");

        // What our own probe_css_accent() makes of the same declaration.
        {
            let provider = gtk::CssProvider::new();
            provider.load_from_string(".myna-probe2 { color: @accent_bg_color; }");
            gtk::style_context_add_provider_for_display(
                &gtk::gdk::Display::default().unwrap(),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            let w = gtk::Label::new(None);
            w.add_css_class("myna-probe2");
            let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
            b.append(&w);
            win.set_child(Some(&b));
            println!("-- myna_hud::platform --");
            println!(
                "  probe_css_accent         -> {:?}",
                myna_hud::platform::probe_css_accent(&w)
            );
            println!(
                "  probe_accent_palette     -> {:?}",
                myna_hud::platform::probe_accent_palette(Some(&w)).main
            );
        }

        let sm = adw::StyleManager::default();
        println!("-- adw property --");
        if let Some(p) = sm.find_property("accent-color-rgba") {
            let v = sm.property_value(p.name());
            if let Ok(c) = v.get::<gtk::gdk::RGBA>() {
                println!(
                    "  accent-color-rgba        -> #{:02x}{:02x}{:02x}",
                    (c.red() * 255.0).round() as u8,
                    (c.green() * 255.0).round() as u8,
                    (c.blue() * 255.0).round() as u8
                );
            }
        }

        std::process::exit(0);
    });
    app.run_with_args::<&str>(&[]);
}
