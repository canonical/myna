//! lab — the development lab and the simulator's control surface (feature
//! 004, T131/T132).
//!
//! The lab is one control window with:
//!
//! * a HUD preview (embedded pill, or an external always-on-top window —
//!   switchable live via the Publish toggle), driven from the controls
//! * the state/level/severity/reduced-motion/color-scheme controls
//! * a dictation target (focus-safety check, FR-024)
//!
//! The **Publish** toggle switches between two modes at runtime:
//!   - **off** (default in `--lab`): the HUD is an external window, not
//!     published; for developing the renderer standalone.
//!   - **on** (default in `--serve-dbus`): the HUD is embedded as a preview,
//!     and the controls are published over `com.canonical.Myna.Dictation` so a
//!     shell-hosted instance shows the real overlay.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::pill::Pill;
use crate::simulator::{default_status_message, envelope_to_levels, PUBLISH_HZ};
use crate::states::{state_to_descriptor, wire};
use crate::window::HudWindow;

/// The lab's live control values.
#[derive(Clone, Debug)]
struct Controls {
    state: String,
    status_message: String,
    envelope: f64,
    reduced_motion: Option<bool>,
    high_contrast: Option<bool>,
    hud_style: Option<crate::hud_logic::HudStyle>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            state: wire::RECORDING.to_string(),
            status_message: default_status_message(wire::RECORDING).to_string(),
            envelope: 0.4,
            reduced_motion: None,
            high_contrast: None,
            hud_style: None,
        }
    }
}

/// Where the lab renders the HUD: a separate overlay window, or a pill
/// embedded in the control window.
enum Target {
    /// An external always-on-top HUD window (Publish off).
    Window(Rc<HudWindow>),
    /// An embedded preview pill (Publish on).
    Embedded(Rc<Pill>),
}

impl Target {
    fn apply_descriptor(&self, descriptor: crate::states::Descriptor) {
        match self {
            Target::Window(w) => w.apply_descriptor(descriptor),
            Target::Embedded(p) => p.apply_descriptor(descriptor),
        }
    }

    fn push_level(&self, rms: f64, peak: f64) {
        match self {
            Target::Window(w) => w.push_level(rms, peak),
            Target::Embedded(p) => p.push_level(rms, peak),
        }
    }

    fn set_reduced_motion_override(&self, value: Option<bool>) {
        match self {
            Target::Window(w) => w.set_reduced_motion_override(value),
            Target::Embedded(p) => p.set_reduced_motion_override(value),
        }
    }

    fn resync_accent(&self) {
        match self {
            Target::Window(w) => w.resync_accent(),
            Target::Embedded(p) => p.resync_accent(),
        }
    }

    fn set_accent_override(&self, hex: Option<String>) {
        match self {
            Target::Window(w) => w.set_accent_override(hex),
            Target::Embedded(p) => p.set_accent_override(hex),
        }
    }

    fn set_high_contrast_override(&self, value: Option<bool>) {
        match self {
            Target::Window(w) => w.set_high_contrast_override(value),
            Target::Embedded(p) => p.set_high_contrast_override(value),
        }
    }

    fn set_hud_style_override(&self, style: Option<crate::hud_logic::HudStyle>) {
        match self {
            Target::Window(w) => w.set_hud_style_override(style),
            Target::Embedded(p) => p.set_hud_style(style),
        }
    }

    fn current_wire_state(&self) -> String {
        match self {
            Target::Window(w) => w.current_wire_state(),
            Target::Embedded(p) => p.current_wire_state(),
        }
    }
}

/// `--lab`: the HUD as an external window, no backend.
pub fn present(app: &adw::Application) {
    build_lab(app, false);
}

/// `--serve-dbus`: the HUD embedded as a preview, publishing to the bus so a
/// shell-hosted instance shows the real overlay.
pub fn present_serving(app: &adw::Application) {
    build_lab(app, true);
}

fn build_lab(app: &adw::Application, publishing: bool) {
    let shared = Rc::new(crate::serve::Shared::default());

    // The Publish toggle switches between an external window (no bus) and an
    // embedded preview + publishing. The `Shared` is always available so
    // the toggle can claim/release the name without re-creating it.
    let publisher = Rc::new(RefCell::new(PublisherState::Unclaimed));

    // Start the serve loop if launching in publish mode.
    if publishing {
        start_publish(shared.clone(), &publisher);
    }

    let controls = Rc::new(RefCell::new(Controls::default()));
    let target: Rc<RefCell<Target>> = Rc::new(RefCell::new(if publishing {
        Target::Embedded(Pill::new())
    } else {
        let hud = HudWindow::new(app);
        hud.present_standalone();
        Target::Window(hud)
    }));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("myna HUD lab")
        .default_width(460)
        .default_height(620)
        .build();

    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.set_margin_top(16);
    page.set_margin_bottom(16);
    page.set_margin_start(16);
    page.set_margin_end(16);

    // ── Embedded HUD preview (shown only while publishing) ─────────────
    // Placed directly below the publish row — it is "what the toggle is
    // doing", not a separate section. Declared here but appended after the
    // publish row.
    let preview_frame = gtk::Frame::new(Some("Published to com.canonical.Myna.Dictation"));
    let preview_holder = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    preview_holder.set_halign(gtk::Align::Center);
    preview_holder.set_margin_top(8);
    preview_holder.set_margin_bottom(8);
    preview_frame.set_child(Some(&preview_holder));
    sync_preview(&preview_holder, &target.borrow());
    if !publishing {
        preview_frame.set_visible(false);
    }

    // ── GNOME Shell ─────────────────────────────────────────────────────
    // First control group: the publish toggle and its preview, so the
    // "am I driving the bus?" question is answered before the model inputs.
    let publish_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(publishing)
        .build();
    let publish_row = adw::ActionRow::builder()
        .title("Publish on the session bus")
        .subtitle(
            "on: the HUD is embedded and published; a shell-hosted instance shows the real overlay",
        )
        .build();
    publish_row.add_suffix(&publish_switch);
    publish_row.set_activatable_widget(Some(&publish_switch));

    let shell_group = adw::PreferencesGroup::new();
    shell_group.add(&publish_row);
    page.append(&shell_group);
    page.append(&preview_frame);

    // ── State ───────────────────────────────────────────────────────────
    let states = gtk::StringList::new(&wire::ALL);
    let state_row = adw::ComboRow::builder()
        .title("State")
        .subtitle("the wire value the publisher would send")
        .model(&states)
        .build();
    state_row.set_selected(
        wire::ALL
            .iter()
            .position(|s| *s == wire::RECORDING)
            .unwrap_or(0) as u32,
    );

    let status_message_row = adw::EntryRow::new();
    status_message_row.set_title("Status message");
    status_message_row.set_text(default_status_message(wire::RECORDING));

    // ── Level ───────────────────────────────────────────────────────────
    let level = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    level.set_value(0.4);
    level.set_hexpand(true);
    level.set_draw_value(true);
    let level_row = adw::ActionRow::builder()
        .title("Audio level")
        .subtitle("the smoothed envelope, published as RMS/peak")
        .build();
    level_row.add_suffix(&level);

    let model_group = adw::PreferencesGroup::new();
    model_group.add(&state_row);
    model_group.add(&status_message_row);
    model_group.add(&level_row);
    page.append(&model_group);

    // ── Display ─────────────────────────────────────────────────────────
    // Reduced-motion override (accessibility path, FR-022a), color scheme,
    // high-contrast (FR-022) and accent — all lab-only overrides of desktop
    // preferences, so legibility can be checked without changing the system.
    let reduced_motion = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    let reduced_motion_row = adw::ActionRow::builder()
        .title("Reduced motion")
        .subtitle("the static/minimal-motion accessibility path")
        .build();
    reduced_motion_row.add_suffix(&reduced_motion);

    let high_contrast = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    let high_contrast_row = adw::ActionRow::builder()
        .title("High contrast")
        .subtitle("bright border / background for busy wallpaper")
        .build();
    high_contrast_row.add_suffix(&high_contrast);

    let color_scheme = gtk::StringList::new(&["default", "light", "dark"]);
    let color_scheme_row = adw::ComboRow::builder()
        .title("Color scheme")
        .subtitle("force light/dark to check legibility")
        .model(&color_scheme)
        .build();
    color_scheme_row.set_selected(0);

    // Accent override: libadwaita has no public runtime accent setter (it is
    // a desktop preference), so the lab forces the palette directly. The
    // options are derived from libadwaita's OWN enum + theme rather than
    // hardcoded: the names come from the AdwAccentColor GType enum nicks,
    // and each value is resolved at runtime from the theme's CSS
    // `--accent-<name>` variable — so the list always matches the running
    // libadwaita (including Ubuntu's Yaru values and any future accents).
    let accent_options = build_accent_options();
    let accent_labels: Vec<&str> = accent_options.iter().map(|(n, _)| n.as_str()).collect();
    let accent_hexes: Vec<Option<String>> = accent_options.iter().map(|(_, h)| h.clone()).collect();
    let accent_model = gtk::StringList::new(&accent_labels);
    let accent_row = adw::ComboRow::builder()
        .title("Accent color")
        .subtitle("force the pill's accent (libadwaita has no runtime setter)")
        .model(&accent_model)
        .build();
    accent_row.set_selected(0);

    // Indicator style: the GSettings-backed `hud-style` (bar / ribbon /
    // vumeter), overridable here for previewing each without touching the
    // desktop store. `default` re-reads the desktop value (now `bar`).
    let hud_style_model =
        gtk::StringList::new(&["default", "bar", "ribbon", "vumeter", "progress"]);
    let hud_style_row = adw::ComboRow::builder()
        .title("Indicator style")
        .subtitle("bar (accent level), ribbon (GPU wave), vumeter (segmented meter) or progress (GtkProgressBar)")
        .model(&hud_style_model)
        .build();
    hud_style_row.set_selected(0);

    let display_group = adw::PreferencesGroup::new();
    display_group.add(&reduced_motion_row);
    display_group.add(&high_contrast_row);
    display_group.add(&color_scheme_row);
    display_group.add(&accent_row);
    display_group.add(&hud_style_row);
    page.append(&display_group);

    accent_row.connect_selected_notify({
        let target = target.clone();
        move |row| {
            let idx = row.selected() as usize;
            let hex = accent_hexes.get(idx).cloned().flatten();
            target.borrow().set_accent_override(hex);
        }
    });

    hud_style_row.connect_selected_notify({
        let target = target.clone();
        let controls = controls.clone();
        move |row| {
            let style = match row.selected() {
                1 => Some(crate::hud_logic::HudStyle::Bar),
                2 => Some(crate::hud_logic::HudStyle::Ribbon),
                3 => Some(crate::hud_logic::HudStyle::Vumeter),
                4 => Some(crate::hud_logic::HudStyle::Progress),
                _ => None,
            };
            controls.borrow_mut().hud_style = style;
            target.borrow().set_hud_style_override(style);
        }
    });

    // ── Dictation target (focus safety, FR-024) ─────────────────────────
    let dictation_target = gtk::TextView::new();
    dictation_target.set_wrap_mode(gtk::WrapMode::WordChar);
    dictation_target
        .buffer()
        .set_text("Type here while the HUD is showing: the caret must never move to the HUD.");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&dictation_target)
        .vexpand(true)
        .has_frame(true)
        .build();
    let target_label = gtk::Label::new(Some("Dictation target"));
    target_label.set_xalign(0.0);
    target_label.add_css_class("heading");
    page.append(&target_label);
    page.append(&scroller);

    // Composed by hand rather than with adw::ToolbarView, which would require
    // a libadwaita 1.4 compile-time feature; the runtime matrix keeps this
    // crate's adw feature floor at 1.0 (R26).
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    shell.append(&adw::HeaderBar::new());
    shell.append(&page);
    window.set_content(Some(&shell));

    // ── Wiring ──────────────────────────────────────────────────────────
    let apply = {
        let target = target.clone();
        let controls = controls.clone();
        let shared = shared.clone();
        move || {
            let controls = controls.borrow();
            // Publish the controls to the bus if we are serving.
            shared.set_controls(crate::serve::Controls {
                state: controls.state.clone(),
                status_message: controls.status_message.clone(),
                envelope: controls.envelope,
            });
            target.borrow().apply_descriptor(state_to_descriptor(
                Some(&controls.state),
                &controls.status_message,
            ));
        }
    };

    state_row.connect_selected_notify({
        let controls = controls.clone();
        let apply = apply.clone();
        let status_message_row = status_message_row.clone();
        move |row| {
            let index = row.selected() as usize;
            if let Some(state) = wire::ALL.get(index) {
                let message = default_status_message(state);
                {
                    let mut controls = controls.borrow_mut();
                    controls.state = (*state).to_string();
                    controls.status_message = message.to_string();
                }
                // set_text() synchronously emits `changed`, whose handler
                // borrows `controls`; release the mutable borrow first.
                status_message_row.set_text(message);
            }
            apply();
        }
    });

    status_message_row.connect_changed({
        let controls = controls.clone();
        let apply = apply.clone();
        move |entry| {
            controls.borrow_mut().status_message = entry.text().to_string();
            apply();
        }
    });

    level.connect_value_changed({
        let controls = controls.clone();
        move |scale| controls.borrow_mut().envelope = scale.value()
    });

    reduced_motion.connect_active_notify({
        let target = target.clone();
        let controls = controls.clone();
        move |switch| {
            let value = if switch.is_active() { Some(true) } else { None };
            controls.borrow_mut().reduced_motion = value;
            target.borrow().set_reduced_motion_override(value);
        }
    });

    high_contrast.connect_active_notify({
        let target = target.clone();
        let controls = controls.clone();
        move |switch| {
            let value = if switch.is_active() { Some(true) } else { None };
            controls.borrow_mut().high_contrast = value;
            target.borrow().set_high_contrast_override(value);
        }
    });

    color_scheme_row.connect_selected_notify({
        move |row| {
            let scheme = match row.selected() {
                1 => adw::ColorScheme::ForceLight,
                2 => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            };
            adw::StyleManager::default().set_color_scheme(scheme);
        }
    });

    // The Publish toggle switches between external window (off) and embedded
    // preview + bus publishing (on). This is a live swap of the `Target`.
    {
        let app = app.clone();
        let target = target.clone();
        let shared = shared.clone();
        let publisher = publisher.clone();
        let controls = controls.clone();
        let preview_frame = preview_frame.clone();
        let preview_holder = preview_holder.clone();
        let apply = apply.clone();
        publish_switch.connect_active_notify(move |switch| {
            let publishing = switch.is_active();
            swap_target(&app, &target, &preview_holder, &preview_frame, publishing);
            if publishing {
                start_publish(shared.clone(), &publisher);
            } else {
                stop_publish(&shared, &publisher);
            }
            // Re-sync reduced-motion, high-contrast, indicator style and
            // accent onto the new target.
            let rm = controls.borrow().reduced_motion;
            target.borrow().set_reduced_motion_override(rm);
            let hc = controls.borrow().high_contrast;
            target.borrow().set_high_contrast_override(hc);
            let style = controls.borrow().hud_style;
            target.borrow().set_hud_style_override(style);
            target.borrow().resync_accent();
            apply();
        });
    }

    // Publish levels at the contract's cadence (C4), driving both the local
    // target and the bus (if publishing).
    glib::timeout_add_local(
        Duration::from_secs_f64(1.0 / PUBLISH_HZ),
        glib::clone!(
            #[strong]
            target,
            #[strong]
            controls,
            #[strong]
            shared,
            move || {
                let controls = controls.borrow();
                if controls.state != wire::IDLE {
                    let (rms, peak) = envelope_to_levels(controls.envelope);
                    target.borrow().push_level(rms, peak);
                    // The sink drives the bus Shared each tick so the
                    // shell-hosted instance's AudioRms tracks the slider.
                    shared.set_controls(crate::serve::Controls {
                        state: controls.state.clone(),
                        status_message: controls.status_message.clone(),
                        envelope: controls.envelope,
                    });
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    apply();

    // Keep lab combo in sync when HUD auto-dismisses locally (notifier-side
    // timeout). The HUD's NoticeSlot returns to idle after its dynamic hold
    // without a new bus publish; the lab's Controls and combo must follow
    // so the next publish is idle and the UI reflects reality.
    glib::timeout_add_local(
        Duration::from_millis(200),
        glib::clone!(
            #[strong]
            target,
            #[strong]
            controls,
            #[strong]
            state_row,
            move || {
                let current = target.borrow().current_wire_state();
                if controls.borrow().state != current {
                    if let Some(pos) = wire::ALL.iter().position(|s| *s == current) {
                        let cur_sel = state_row.selected() as usize;
                        if cur_sel != pos {
                            controls.borrow_mut().state = current.clone();
                            state_row.set_selected(pos as u32);
                        } else {
                            controls.borrow_mut().state = current;
                        }
                    } else {
                        controls.borrow_mut().state = current;
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    window.present();
}

/// The publisher's live state: idle (no bus name) or serving (owns
/// `com.canonical.Myna.Dictation`).
#[derive(Debug)]
enum PublisherState {
    /// Never claimed the name (no bus, or --lab).
    Unclaimed,
    /// The name is owned; `publishing` gates whether the loop emits live
    /// state or forces idle.
    Claimed,
}

/// Claim the bus name once. The connection is held for the process
/// lifetime (releasing and re-claiming in the same process races — the
/// detached publish loop keeps the old connection alive). Instead,
/// publishing is gated via `Shared::set_publishing`, which makes the
/// snapshot force idle when off — the same observable effect as releasing
/// the name, without the race.
fn start_publish(shared: Rc<crate::serve::Shared>, publisher: &Rc<RefCell<PublisherState>>) {
    // The name is claimed exactly once for the process lifetime (the
    // detached publish loop keeps the connection alive). The PUBLISH GATE is
    // separate and must re-enable on EVERY toggle-on — the early return
    // below only skips the claim, not the gate, otherwise re-enabling after
    // a stop_publish() would silently do nothing (the pill would never come
    // back until the lab restarted).
    if matches!(*publisher.borrow(), PublisherState::Unclaimed) {
        *publisher.borrow_mut() = PublisherState::Claimed;
        let shared = (*shared).clone();
        glib::spawn_future_local(async move {
            match crate::serve::serve(shared).await {
                Ok(connection) => {
                    std::mem::forget(connection); // held for process lifetime
                    eprintln!("myna-hud: publishing com.canonical.Myna.Dictation");
                }
                Err(e) => {
                    eprintln!("myna-hud: {e}");
                }
            }
        });
    }
    // Always re-enable the gate, claimed or not.
    shared.set_publishing(true);
}

/// Stop publishing: gate the snapshot to idle without releasing the name.
fn stop_publish(shared: &crate::serve::Shared, _publisher: &Rc<RefCell<PublisherState>>) {
    shared.set_publishing(false);
}

/// Swap the HUD target between an external window and an embedded pill.
fn swap_target(
    app: &adw::Application,
    target: &RefCell<Target>,
    preview_holder: &gtk::Box,
    preview_frame: &gtk::Frame,
    publishing: bool,
) {
    // Tear down the old target: remove the embedded pill's widget, or
    // CLOSE the external HUD window (otherwise it lingers, still showing,
    // while the preview takes over — two HUDs on screen).
    match &*target.borrow() {
        Target::Embedded(_) => {
            while let Some(child) = preview_holder.first_child() {
                preview_holder.remove(&child);
            }
        }
        Target::Window(hud) => {
            hud.window().close();
        }
    }

    let new_target = if publishing {
        preview_frame.set_visible(true);
        let pill = Pill::new();
        preview_holder.append(pill.widget());
        Target::Embedded(pill)
    } else {
        preview_frame.set_visible(false);
        let hud = HudWindow::new(app);
        hud.present_standalone();
        Target::Window(hud)
    };
    *target.borrow_mut() = new_target;
}

/// Populate the preview holder from the current target (if embedded).
fn sync_preview(preview_holder: &gtk::Box, target: &Target) {
    if let Target::Embedded(pill) = target {
        preview_holder.append(pill.widget());
    }
}

/// Build the lab's accent-option list from libadwaita itself.
///
/// The names come from the `AdwAccentColor` GType enum's member nicks, and
/// each value is resolved at runtime from the theme's CSS
/// `--accent-<nick>` custom property (read back from a probe widget's
/// computed colour) — the same runtime-CSS technique the pill once used,
/// now confined to the lab. This way the list always matches the running
/// libadwaita: Ubuntu's Yaru values, any future accent, and the exact
/// names the enum exposes.
///
/// The first option is "default" (no override). Each following option is
/// `(display label, Some(hex))`.
pub fn build_accent_options() -> Vec<(String, Option<String>)> {
    let mut options = vec![("default".to_string(), None)];

    // Enumerate AdwAccentColor's members from its GType.
    let class = glib::EnumClass::new::<adw::AccentColor>();
    for member in class.values() {
        let nick = member.nick();
        // The theme's CSS variable for this accent: `--accent-blue`, etc.
        let variable = format!("--accent-{nick}");
        // A probe widget styled with the variable, read back as a colour.
        // Needs to be attached to a display to have a style context.
        let Some(hex) = css_color_hex(&variable) else {
            // The theme didn't define the variable (older libadwaita):
            // still list the accent, but without a usable override value.
            options.push((nick.to_string(), None));
            continue;
        };
        options.push((nick.to_string(), Some(hex)));
    }
    options
}

/// Resolve a CSS custom property's colour (e.g. `--accent-blue`) through the
/// current theme, as a `#rrggbb` hex. `None` if the property is not defined.
///
/// Uses one shared probe widget rooted in a hidden window: the widget's
/// computed colour is read back after re-loading the provider with the
/// variable, the way accent_css_probe() demonstrated works synchronously.
/// A fully transparent result means the theme did not define the variable
/// (older libadwaita).
fn css_color_hex(variable: &str) -> Option<String> {
    let display = gdk::Display::default()?;

    let provider = gtk::CssProvider::new();
    provider.load_from_string(&format!(
        ".myna-lab-accent-probe {{ color: var({variable}); }}"
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    // Root the probe in a (non-app) window so it has a style context. A
    // window without an application can still compute style once presented.
    let probe = gtk::Label::new(None);
    probe.add_css_class("myna-lab-accent-probe");
    let window = gtk::Window::new();
    window.set_child(Some(&probe));
    window.present();

    let color = probe.color();
    window.close();
    if color.alpha() <= 0.0 {
        return None;
    }
    Some(format!(
        "#{:02x}{:02x}{:02x}",
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8
    ))
}
