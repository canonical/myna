//! lab — the development lab and the simulator's control surface (feature
//! 004, T131/T132), replacing the extension's `dev-lab/` and `dev-lab-gpu/`
//! harnesses.
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
//!     and the controls are published over `org.myna.Dictation` so a
//!     shell-hosted instance shows the real overlay.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gettextrs::gettext;
use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::pill::Pill;
use crate::simulator::{envelope_to_levels, ERROR_REASON, NOTICE_REASON, PUBLISH_HZ};
use crate::states::{state_to_descriptor, wire};
use crate::window::HudWindow;

/// The lab's live control values.
#[derive(Clone, Debug)]
struct Controls {
    state: String,
    reason: String,
    envelope: f64,
    reduced_motion: Option<bool>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            state: wire::RECORDING.to_string(),
            reason: String::new(),
            envelope: 0.4,
            reduced_motion: None,
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
        .title(gettext("myna HUD lab"))
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
    let preview_frame = gtk::Frame::new(Some(&gettext("Published to org.myna.Dictation")));
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
        .title(gettext("Publish on the session bus"))
        .subtitle(gettext(
            "on: the HUD is embedded and published; a shell-hosted instance shows the real overlay",
        ))
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
        .title(gettext("State"))
        .subtitle(gettext("the wire value the publisher would send"))
        .model(&states)
        .build();
    state_row.set_selected(
        wire::ALL
            .iter()
            .position(|s| *s == wire::RECORDING)
            .unwrap_or(0) as u32,
    );

    // ── Level ───────────────────────────────────────────────────────────
    let level = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    level.set_value(0.4);
    level.set_hexpand(true);
    level.set_draw_value(true);
    let level_row = adw::ActionRow::builder()
        .title(gettext("Audio level"))
        .subtitle(gettext("the smoothed envelope, published as RMS/peak"))
        .build();
    level_row.add_suffix(&level);

    let model_group = adw::PreferencesGroup::new();
    model_group.add(&state_row);
    model_group.add(&level_row);
    page.append(&model_group);

    // ── Display ─────────────────────────────────────────────────────────
    // Reduced-motion override (accessibility path, FR-022a) and color
    // scheme (light/dark) — both are lab-only overrides of desktop
    // preferences, so the ribbon's legibility can be checked without
    // changing the system.
    let reduced_motion = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(false)
        .build();
    let reduced_motion_row = adw::ActionRow::builder()
        .title(gettext("Reduced motion"))
        .subtitle(gettext("the static/minimal-motion accessibility path"))
        .build();
    reduced_motion_row.add_suffix(&reduced_motion);

    let color_scheme = gtk::StringList::new(&["default", "light", "dark"]);
    let color_scheme_row = adw::ComboRow::builder()
        .title(gettext("Color scheme"))
        .subtitle(gettext("force light/dark to check legibility"))
        .model(&color_scheme)
        .build();
    color_scheme_row.set_selected(0);

    let display_group = adw::PreferencesGroup::new();
    display_group.add(&reduced_motion_row);
    display_group.add(&color_scheme_row);
    page.append(&display_group);

    // ── Dictation target (focus safety, FR-024) ─────────────────────────
    let dictation_target = gtk::TextView::new();
    dictation_target.set_wrap_mode(gtk::WrapMode::WordChar);
    dictation_target.buffer().set_text(&gettext(
        "Type here while the HUD is showing: the caret must never move to the HUD.",
    ));
    let scroller = gtk::ScrolledWindow::builder()
        .child(&dictation_target)
        .vexpand(true)
        .has_frame(true)
        .build();
    let target_label = gtk::Label::new(Some(&gettext("Dictation target")));
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
                reason: match controls.state.as_str() {
                    wire::NOTICE => NOTICE_REASON.to_string(),
                    wire::ERROR => ERROR_REASON.to_string(),
                    _ => controls.reason.clone(),
                },
                envelope: controls.envelope,
            });
            let (state, reason) = match controls.state.as_str() {
                wire::NOTICE => (wire::NOTICE.to_string(), NOTICE_REASON.to_string()),
                wire::ERROR => (wire::ERROR.to_string(), ERROR_REASON.to_string()),
                other => (other.to_string(), String::new()),
            };
            let reason = if controls.reason.is_empty() {
                reason
            } else {
                controls.reason.clone()
            };
            target
                .borrow()
                .apply_descriptor(state_to_descriptor(Some(&state), &reason));
        }
    };

    state_row.connect_selected_notify({
        let controls = controls.clone();
        let apply = apply.clone();
        move |row| {
            let index = row.selected() as usize;
            if let Some(state) = wire::ALL.get(index) {
                controls.borrow_mut().state = (*state).to_string();
            }
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
            // Re-sync reduced-motion and accent onto the new target.
            let rm = controls.borrow().reduced_motion;
            target.borrow().set_reduced_motion_override(rm);
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
                        reason: match controls.state.as_str() {
                            wire::NOTICE => NOTICE_REASON.to_string(),
                            wire::ERROR => ERROR_REASON.to_string(),
                            _ => controls.reason.clone(),
                        },
                        envelope: controls.envelope,
                    });
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    apply();
    window.present();
}

/// The publisher's live state: idle (no bus name) or serving (owns
/// `org.myna.Dictation`).
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
                    eprintln!("myna-hud: publishing org.myna.Dictation");
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
