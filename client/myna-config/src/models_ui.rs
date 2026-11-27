//! The Models groups of Diagnostics: every installed model with its size on
//! disk and Remove, to free disk, and the ways to add more. The list comes
//! from `installed_models`; this module only binds it to GTK and runs the
//! removals, the same way Processing removes a text model.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::command::CancellationToken;
use crate::diagnostics::InstalledSnap;
use crate::installed_models::{self, Inputs, Model, Models, Removal};
use crate::markup::escape_markup;
use crate::plugin_catalog::Entry;
use crate::plugin_install::{self, Failure};
use crate::ports::{DictationDaemon, SystemConfigurator, SystemConfiguratorError};
use crate::ui::InstallControl;

pub struct Deps {
    pub configurator: Rc<dyn SystemConfigurator>,
    pub daemon: Rc<dyn DictationDaemon>,
    pub overlay: adw::ToastOverlay,
    pub follow_interval: Duration,
    pub catalog: Box<dyn Fn() -> Vec<Entry>>,
}

/// What discovery last found, for the list.
pub struct Found<'a> {
    pub snaps: &'a [InstalledSnap],
    pub speech: &'a [String],
    pub connected: Option<&'a str>,
    /// Another install or change of models runs elsewhere in Settings.
    pub busy: bool,
}

struct ModelRow {
    id: String,
    /// The group it was added to: removing it from the other one is a
    /// libadwaita critical.
    group: adw::PreferencesGroup,
    row: adw::ActionRow,
    /// What the row shows now, to patch it when only that changed.
    model: Model,
    /// Remove, then "Removing…" with a spinner: the Install control of
    /// Install more models. `None` on the model in use.
    control: Option<InstallControl>,
}

type Callback = RefCell<Option<Box<dyn Fn()>>>;

pub struct ModelsSection {
    deps: Deps,
    speech_group: adw::PreferencesGroup,
    text_group: adw::PreferencesGroup,
    add_speech: adw::ActionRow,
    add_text: adw::ActionRow,
    rows: RefCell<Vec<ModelRow>>,
    shown: RefCell<Option<Models>>,
    catalog: RefCell<Option<Vec<Entry>>>,
    /// The model being removed.
    removing: RefCell<Option<String>>,
    busy: Cell<bool>,
    on_add_speech: Callback,
    on_add_text: Callback,
    on_removed: Callback,
    cancellation: CancellationToken,
}

/// "Translation · 33 MB": what a model is and what it takes on disk.
fn subtitle(model: &Model) -> String {
    // TRANSLATORS: {kind} says what a model is, such as "Translation" or "Speech recognition", and {size} its size on disk, such as "729 MB". The spaces around the dot are no-break spaces.
    let frame = gettextrs::gettext("{kind}\u{a0}·\u{a0}{size}");
    frame
        .replace("{kind}", &model.kind)
        .replace("{size}", &glib::format_size(model.bytes))
}

fn add_row(title: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(escape_markup(title))
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("list-add-symbolic"));
    row
}

impl ModelsSection {
    pub fn new(deps: Deps) -> Rc<Self> {
        let speech_group = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Speech models"))
            .build();
        let text_group = adw::PreferencesGroup::builder()
            .title(gettextrs::gettext("Text models"))
            .description(gettextrs::gettext(
                "A chain step whose model is removed stays and offers to install it again.",
            ))
            .build();
        let add_speech = add_row(&gettextrs::gettext("Add speech model…"));
        let add_text = add_row(&gettextrs::gettext("Add text model…"));
        speech_group.add(&add_speech);
        text_group.add(&add_text);
        let section = Rc::new(Self {
            deps,
            speech_group,
            text_group,
            add_speech,
            add_text,
            rows: RefCell::new(Vec::new()),
            shown: RefCell::new(None),
            catalog: RefCell::new(None),
            removing: RefCell::new(None),
            busy: Cell::new(false),
            on_add_speech: RefCell::new(None),
            on_add_text: RefCell::new(None),
            on_removed: RefCell::new(None),
            cancellation: CancellationToken::new(),
        });
        let call = |section: &Rc<Self>, pick: fn(&Self) -> &Callback| {
            let section = Rc::downgrade(section);
            move |_: &adw::ActionRow| {
                if let Some(section) = section.upgrade() {
                    if let Some(callback) = pick(&section).borrow().as_ref() {
                        callback();
                    }
                }
            }
        };
        section
            .add_speech
            .connect_activated(call(&section, |s| &s.on_add_speech));
        section
            .add_text
            .connect_activated(call(&section, |s| &s.on_add_text));
        section
    }

    pub fn groups(&self) -> [&adw::PreferencesGroup; 2] {
        [&self.speech_group, &self.text_group]
    }

    pub fn connect_add_speech(&self, callback: impl Fn() + 'static) {
        self.on_add_speech.replace(Some(Box::new(callback)));
    }

    pub fn connect_add_text(&self, callback: impl Fn() + 'static) {
        self.on_add_text.replace(Some(Box::new(callback)));
    }

    /// No catalog to open (the settings store is missing): no Add row.
    pub fn add_text_unavailable(&self) {
        self.add_text.set_visible(false);
    }

    /// Called once a removal is done, so the caller reads snapd again.
    pub fn connect_removed(&self, callback: impl Fn() + 'static) {
        self.on_removed.replace(Some(Box::new(callback)));
    }

    /// Stop following a removal when the window closes; snapd carries on.
    pub fn shutdown(&self) {
        self.cancellation.cancel();
    }

    #[cfg(test)]
    pub(crate) fn stopped(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// List what discovery found. Rows are rebuilt only when the models
    /// changed, so a focus rediscovery that found nothing new leaves them.
    pub fn show(self: &Rc<Self>, found: &Found) {
        self.busy.set(found.busy);
        let models = {
            let catalog = self.catalog_for(found.snaps);
            installed_models::models(&Inputs {
                snaps: found.snaps,
                speech: found.speech,
                connected: found.connected,
                catalog: &catalog,
            })
        };
        if self.shown.borrow().as_ref() != Some(&models) {
            if !self.patch(&models) {
                self.rebuild(&models);
            }
            self.shown.replace(Some(models));
        }
        self.render();
    }

    /// The catalog, read again only when an installed text model is
    /// missing from it (a snap refreshed with new models).
    fn catalog_for(&self, snaps: &[InstalledSnap]) -> Vec<Entry> {
        let known = |catalog: &[Entry]| {
            snaps
                .iter()
                .filter(|s| crate::plugin_catalog::SNAPS.contains(&s.name.as_str()))
                .flat_map(|s| {
                    s.components
                        .iter()
                        .filter(|c| c.installed())
                        .map(move |c| (s, c))
                })
                .all(|(s, c)| {
                    catalog
                        .iter()
                        .any(|e| e.snap == s.name && e.component == c.name)
                })
        };
        let mut cached = self.catalog.borrow_mut();
        if !cached.as_deref().is_some_and(known) {
            *cached = Some((self.deps.catalog)());
        }
        cached.clone().unwrap_or_default()
    }

    /// Bring the rows to `models` without rebuilding any that stay, when
    /// the change is only models gone or a shown detail changed: a removal
    /// then costs the other rows no redraw. False when a row would have to
    /// be added or changed what it offers; the caller rebuilds.
    fn patch(&self, models: &Models) -> bool {
        let wanted: Vec<&Model> = models.speech.iter().chain(&models.text).collect();
        let mut rows = self.rows.borrow_mut();
        let mut next = wanted.iter().peekable();
        for row in rows.iter() {
            let kept = next.next_if(|m| m.id == row.id);
            if kept.is_some_and(|m| m.in_use != row.model.in_use) {
                return false;
            }
        }
        if next.next().is_some() {
            return false;
        }
        rows.retain(|row| {
            let Some(model) = wanted.iter().find(|m| m.id == row.id) else {
                row.group.remove(&row.row);
                return false;
            };
            if **model != row.model {
                row.row.set_title(&escape_markup(&model.title));
                let subtitle = subtitle(model);
                row.row.set_subtitle(&escape_markup(&subtitle));
                row.row
                    .update_property(&[gtk::accessible::Property::Description(&subtitle)]);
            }
            true
        });
        for (row, model) in rows.iter_mut().zip(&wanted) {
            row.model = (*model).clone();
        }
        // TRANSLATORS: {size} is a size on disk, such as "5.2 GB".
        let frame = gettextrs::gettext("Models use {size} of disk in all.");
        self.speech_group.set_description(Some(
            &frame.replace("{size}", &glib::format_size(models.total())),
        ));
        true
    }

    fn rebuild(self: &Rc<Self>, models: &Models) {
        for model in self.rows.take() {
            model.group.remove(&model.row);
        }
        self.speech_group.remove(&self.add_speech);
        self.text_group.remove(&self.add_text);
        // TRANSLATORS: {size} is a size on disk, such as "5.2 GB".
        let frame = gettextrs::gettext("Models use {size} of disk in all.");
        self.speech_group.set_description(Some(
            &frame.replace("{size}", &glib::format_size(models.total())),
        ));
        let mut rows = Vec::new();
        for (group, list) in [
            (&self.speech_group, &models.speech),
            (&self.text_group, &models.text),
        ] {
            for model in list {
                let built = self.model_row(model, group);
                group.add(&built.row);
                rows.push(built);
            }
        }
        self.speech_group.add(&self.add_speech);
        self.text_group.add(&self.add_text);
        self.rows.replace(rows);
    }

    fn model_row(self: &Rc<Self>, model: &Model, group: &adw::PreferencesGroup) -> ModelRow {
        let subtitle = subtitle(model);
        let row = adw::ActionRow::builder()
            .title(escape_markup(&model.title))
            .subtitle(escape_markup(&subtitle))
            .build();
        row.update_property(&[gtk::accessible::Property::Description(&subtitle)]);
        let control = if model.in_use {
            let label = gtk::Label::builder()
                .label(gettextrs::gettext("In use"))
                .valign(gtk::Align::Center)
                .halign(gtk::Align::End)
                .xalign(1.0)
                .width_request(136)
                .css_classes(["dim-label"])
                .build();
            row.add_suffix(&label);
            None
        } else {
            let control = InstallControl::new();
            control.show_offer(
                &gettextrs::gettext("Remove"),
                // TRANSLATORS: {model} names a model, such as "Whisper" or "English to Italiano".
                &gettextrs::gettext("Remove {model}").replace("{model}", &model.title),
            );
            control.button().connect_clicked({
                let section = Rc::downgrade(self);
                let id = model.id.clone();
                move |_| {
                    if let Some(section) = section.upgrade() {
                        section.start_remove(&id);
                    }
                }
            });
            row.add_suffix(&control);
            Some(control)
        };
        ModelRow {
            id: model.id.clone(),
            group: group.clone(),
            row,
            model: model.clone(),
            control,
        }
    }

    pub(crate) fn blocked(&self) -> bool {
        self.busy.get() || self.removing.borrow().is_some()
    }

    fn render(&self) {
        let removing = self.removing.borrow().clone();
        let blocked = self.blocked();
        for model in self.rows.borrow().iter() {
            let this = removing.as_deref() == Some(model.id.as_str());
            let Some(control) = &model.control else {
                continue;
            };
            if this {
                control.show_installing(None);
                control
                    .progress()
                    .label
                    .set_label(&gettextrs::gettext("Removing…"));
            } else {
                control.show_offer(
                    &gettextrs::gettext("Remove"),
                    // TRANSLATORS: {model} names a model, such as "Whisper" or "English to Italiano".
                    &gettextrs::gettext("Remove {model}").replace("{model}", &model.model.title),
                );
                control.button().set_sensitive(!blocked);
            }
        }
    }

    fn start_remove(self: &Rc<Self>, id: &str) {
        if self.blocked() {
            return;
        }
        let Some(model) = self
            .shown
            .borrow()
            .as_ref()
            .and_then(|models| models.find(id).cloned())
        else {
            return;
        };
        if model.in_use {
            return;
        }
        self.removing.replace(Some(model.id.clone()));
        self.render();
        let section = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = run_remove(&section, &model.removal).await;
            let Some(section) = section.upgrade() else {
                return;
            };
            section.removing.replace(None);
            section.render();
            match outcome {
                Ok(()) | Err(Failure::Dismissed) => {}
                Err(Failure::Dictating) => {
                    section
                        .deps
                        .overlay
                        .add_toast(adw::Toast::new(&gettextrs::gettext(
                            "Finish dictating, then try again.",
                        )));
                }
                Err(Failure::Failed(error)) => {
                    section.toast_failure(&model.title, &error);
                }
            }
            let removed = section.on_removed.borrow();
            if let Some(callback) = removed.as_ref() {
                callback();
            }
        });
    }

    fn toast_failure(&self, title: &str, error: &SystemConfiguratorError) {
        // TRANSLATORS: {model} names a model, such as "Whisper" or "English to Italiano".
        let frame = gettextrs::gettext("Removing {model} failed");
        let heading = frame.replace("{model}", title);
        crate::backend_ui::failure_toast(&self.deps.overlay, &heading, error);
    }
}

/// Remove one model: its speech snap, or a text snap's components.
async fn run_remove(
    section: &std::rc::Weak<ModelsSection>,
    removal: &Removal,
) -> Result<(), Failure> {
    let Some(strong) = section.upgrade() else {
        return Err(Failure::Dismissed);
    };
    let sleep = |interval: Duration| -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
        Box::pin(glib::timeout_future(interval))
    };
    let follow = crate::snap_install::Follow {
        interval: strong.deps.follow_interval,
        sleep: &sleep,
        cancellation: strong.cancellation.clone(),
    };
    match removal {
        Removal::Snap(snap) => {
            if let Ok(Some(state)) = strong.deps.daemon.state().await {
                if plugin_install::dictating(&state) {
                    return Err(Failure::Dictating);
                }
            }
            crate::snap_install::remove(strong.deps.configurator.as_ref(), snap, &follow).await?;
        }
        Removal::Components(request) => {
            let ctx = plugin_install::Context {
                configurator: strong.deps.configurator.as_ref(),
                daemon: strong.deps.daemon.as_ref(),
                follow: &follow,
            };
            plugin_install::remove(&ctx, request, &|_| {}).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::active_backend::SwitchPlan;
    use crate::backend_apply::ApplyPreview;
    use crate::diagnostics::SnapComponent;
    use crate::domain::CommandResult;
    use crate::ports::{SetUpPlan, SystemConfiguratorFailure};
    use crate::snap_changes::{parse_change, ChangeInProgress};
    use async_trait::async_trait;
    use std::time::Instant;

    struct Daemon(&'static str);

    #[async_trait(?Send)]
    impl DictationDaemon for Daemon {
        async fn state(&self) -> Result<Option<String>, String> {
            Ok(Some(self.0.into()))
        }
        async fn offered_models(&self) -> Result<Option<Vec<String>>, String> {
            Ok(Some(Vec::new()))
        }
        async fn rescan(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// snapd as the user: records each request, answers `remove_snap` with
    /// `answer`, and reports every change it started done.
    struct Snapd {
        calls: Rc<RefCell<Vec<String>>>,
        answer: Result<Option<String>, SystemConfiguratorError>,
    }

    #[async_trait(?Send)]
    impl SystemConfigurator for Snapd {
        async fn execute_backend_switch(
            &self,
            _: &SwitchPlan,
            _: CancellationToken,
        ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
            unreachable!()
        }
        async fn restart_myna(&self, _: CancellationToken) -> Result<(), SystemConfiguratorError> {
            unreachable!()
        }
        async fn apply_backend_config(
            &self,
            _: &ApplyPreview,
            _: CancellationToken,
        ) -> Result<Vec<CommandResult>, SystemConfiguratorFailure> {
            unreachable!()
        }
        async fn user_daemons_enabled(&self, _: CancellationToken) -> Result<bool, String> {
            unreachable!()
        }
        async fn set_up(
            &self,
            _: &SetUpPlan,
            _: CancellationToken,
        ) -> Result<(), SystemConfiguratorError> {
            unreachable!()
        }
        async fn install_snap(
            &self,
            _: &str,
            _: CancellationToken,
        ) -> Result<Option<String>, SystemConfiguratorError> {
            unreachable!("Diagnostics installs nothing itself")
        }
        async fn remove_snap(
            &self,
            snap: &str,
            _: CancellationToken,
        ) -> Result<Option<String>, SystemConfiguratorError> {
            self.calls.borrow_mut().push(format!("remove {snap}"));
            glib::timeout_future(Duration::from_millis(50)).await;
            self.answer.clone()
        }
        async fn change_components(
            &self,
            snap: &str,
            components: &[String],
            install: bool,
            _: CancellationToken,
        ) -> Result<Option<String>, SystemConfiguratorError> {
            assert!(!install);
            self.calls
                .borrow_mut()
                .push(format!("remove {snap}+{}", components.join("+")));
            Ok(Some("9".into()))
        }
        async fn snap_change(
            &self,
            id: &str,
            _: CancellationToken,
        ) -> Result<ChangeInProgress, String> {
            Ok(parse_change(serde_json::json!({
                "id": id, "kind": "remove-snap", "ready": true, "status": "Done",
                "summary": "Remove", "tasks": []}))
            .unwrap())
        }
    }

    struct Rig {
        section: Rc<ModelsSection>,
        calls: Rc<RefCell<Vec<String>>>,
        removed: Rc<Cell<u32>>,
        overlay: adw::ToastOverlay,
        window: gtk::Window,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.window.destroy();
        }
    }

    fn rig(state: &'static str, answer: Result<Option<String>, SystemConfiguratorError>) -> Rig {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let overlay = adw::ToastOverlay::new();
        let section = ModelsSection::new(Deps {
            configurator: Rc::new(Snapd {
                calls: calls.clone(),
                answer,
            }),
            daemon: Rc::new(Daemon(state)),
            overlay: overlay.clone(),
            follow_interval: Duration::from_millis(1),
            catalog: Box::new(Vec::new),
        });
        let page = adw::PreferencesPage::new();
        for group in section.groups() {
            page.add(group);
        }
        overlay.set_child(Some(&page));
        let window = gtk::Window::builder().child(&overlay).build();
        window.present();
        let removed = Rc::new(Cell::new(0));
        section.connect_removed({
            let removed = removed.clone();
            move || removed.set(removed.get() + 1)
        });
        section.show(&found(false));
        Rig {
            section,
            calls,
            removed,
            overlay,
            window,
        }
    }

    /// A section over fakes that remove nothing, for other pages' tests.
    pub(crate) fn idle_section() -> Rc<ModelsSection> {
        ModelsSection::new(Deps {
            configurator: Rc::new(Snapd {
                calls: Rc::default(),
                answer: Ok(None),
            }),
            daemon: Rc::new(Daemon("idle")),
            overlay: adw::ToastOverlay::new(),
            follow_interval: Duration::from_millis(1),
            catalog: Box::new(Vec::new),
        })
    }

    fn snaps() -> Vec<InstalledSnap> {
        let component = |name: &str, bytes: u64| SnapComponent {
            name: name.into(),
            summary: format!("{name} summary"),
            installed_size: bytes,
            install_date: Some("2026-11-23T00:00:00Z".into()),
        };
        vec![
            InstalledSnap {
                name: "myna-parakeet".into(),
                version: "1".into(),
                installed_size: 46_000_000,
                components: vec![component("model-parakeet-int8", 729_000_000)],
            },
            InstalledSnap {
                name: "myna-translation".into(),
                version: "1".into(),
                installed_size: 28_000_000,
                components: vec![component("model-translate-en-it", 33_000_000)],
            },
            InstalledSnap {
                name: "myna-whisper".into(),
                version: "1".into(),
                installed_size: 124_000_000,
                components: Vec::new(),
            },
        ]
    }

    fn found(busy: bool) -> Found<'static> {
        let snaps: &'static [InstalledSnap] = Box::leak(snaps().into_boxed_slice());
        let speech: &'static [String] =
            Box::leak(vec!["myna-parakeet".into(), "myna-whisper".into()].into_boxed_slice());
        Found {
            snaps,
            speech,
            connected: Some("myna-parakeet"),
            busy,
        }
    }

    fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn row(rig: &Rig, title: &str) -> (adw::ActionRow, Option<gtk::Button>) {
        let rows = rig.section.rows.borrow();
        let found = rows
            .iter()
            .find(|r| r.row.title() == title)
            .unwrap_or_else(|| panic!("no row {title}"));
        (
            found.row.clone(),
            found.control.as_ref().map(InstallControl::button),
        )
    }

    fn toasts(rig: &Rig) -> usize {
        // A toast shows as an `AdwToastWidget` child of the overlay.
        let mut count = 0;
        let mut child = rig.overlay.first_child();
        while let Some(widget) = child {
            if widget.type_().name() == "AdwToastWidget" {
                count += 1;
            }
            child = widget.next_sibling();
        }
        count
    }

    #[test]
    fn the_groups_list_each_model_with_its_size_and_the_total() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let titles: Vec<String> = rig
                .section
                .rows
                .borrow()
                .iter()
                .map(|r| r.row.title().to_string())
                .collect();
            assert_eq!(
                titles,
                ["Parakeet", "Whisper", "model-translate-en-it summary"]
            );
            let (parakeet, remove) = row(&rig, "Parakeet");
            assert!(remove.is_none(), "the model in use has no Remove");
            assert_eq!(
                parakeet.subtitle().unwrap(),
                format!(
                    "Speech recognition\u{a0}·\u{a0}{}",
                    glib::format_size(775_000_000)
                )
            );
            let total = glib::format_size(775_000_000 + 124_000_000 + 33_000_000);
            assert_eq!(
                rig.section.speech_group.description().unwrap(),
                format!("Models use {total} of disk in all.")
            );
            let (_, whisper) = row(&rig, "Whisper");
            let whisper = whisper.expect("a Remove");
            assert!(whisper.is_sensitive());
            assert_eq!(whisper.accessible_role(), gtk::AccessibleRole::Button,);
        });
    }

    #[test]
    fn a_rediscovery_that_found_nothing_new_keeps_the_rows() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let (before, _) = row(&rig, "Whisper");
            rig.section.show(&found(false));
            let (after, _) = row(&rig, "Whisper");
            assert_eq!(before, after);
        });
    }

    #[test]
    fn a_removed_model_leaves_only_its_own_row() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let (parakeet, _) = row(&rig, "Parakeet");
            let (text, _) = row(&rig, "model-translate-en-it summary");
            let mut after = found(false);
            let kept: &'static [InstalledSnap] = Box::leak(
                after
                    .snaps
                    .iter()
                    .filter(|s| s.name != "myna-whisper")
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            );
            after.snaps = kept;
            rig.section.show(&after);
            assert_eq!(rig.section.rows.borrow().len(), 2);
            assert_eq!(row(&rig, "Parakeet").0, parakeet);
            assert_eq!(row(&rig, "model-translate-en-it summary").0, text);
            assert!(parakeet.parent().is_some() && text.parent().is_some());
        });
    }

    #[test]
    fn remove_takes_a_speech_family_snap_away_and_rediscovers() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let (_, whisper) = row(&rig, "Whisper");
            whisper.unwrap().emit_clicked();
            assert!(rig.section.blocked(), "one removal at a time");
            pump_until("the removal", || rig.removed.get() == 1);
            assert_eq!(*rig.calls.borrow(), ["remove myna-whisper"]);
            assert!(!rig.section.blocked());
            assert_eq!(toasts(&rig), 0, "a success says nothing");
        });
    }

    #[test]
    fn remove_takes_a_text_component_away_and_leaves_its_snap() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let (_, remove) = row(&rig, "model-translate-en-it summary");
            remove.unwrap().emit_clicked();
            pump_until("the removal", || rig.removed.get() == 1);
            assert_eq!(
                *rig.calls.borrow(),
                ["remove myna-translation+model-translate-en-it"]
            );
        });
    }

    #[test]
    fn nothing_is_removed_while_another_change_runs() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            rig.section.show(&found(true));
            let (_, whisper) = row(&rig, "Whisper");
            let whisper = whisper.unwrap();
            assert!(!whisper.is_sensitive());
            rig.section.start_remove("myna-whisper");
            rig.section.start_remove("myna-parakeet");
            pump_until("idle", || !glib::MainContext::default().pending());
            assert!(rig.calls.borrow().is_empty());
        });
    }

    #[test]
    fn nothing_is_removed_while_dictating() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("recording", Ok(Some("9".into())));
            let (_, whisper) = row(&rig, "Whisper");
            whisper.unwrap().emit_clicked();
            pump_until("the refusal", || rig.removed.get() == 1);
            assert!(rig.calls.borrow().is_empty());
            assert_eq!(toasts(&rig), 1, "says to finish dictating");
        });
    }

    #[test]
    fn a_dismissed_prompt_says_nothing_and_a_failure_offers_details() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Err(SystemConfiguratorError::Cancelled));
            let (_, whisper) = row(&rig, "Whisper");
            whisper.unwrap().emit_clicked();
            pump_until("the removal", || rig.removed.get() == 1);
            assert_eq!(toasts(&rig), 0);

            let rig = rig_failing();
            let (_, whisper) = row(&rig, "Whisper");
            whisper.unwrap().emit_clicked();
            pump_until("the removal", || rig.removed.get() == 1);
            assert_eq!(toasts(&rig), 1);
        });
    }

    fn rig_failing() -> Rig {
        rig(
            "idle",
            Err(SystemConfiguratorError::snapd_execution(
                "POST /v2/snaps/myna-whisper (remove)",
                Some(400),
                "snap is busy",
            )),
        )
    }

    fn catalog_entry() -> Entry {
        serde_json::from_value(serde_json::json!({
            "id": "en-it", "snap": "myna-translation", "component": "model-translate-en-it",
            "model": "m-en-it", "task": "text.translate", "in_lang": "en", "out_lang": "it",
            "step": {"id": "translate", "stage": "translate", "label_key": "step.translate.name",
                     "description_key": "", "example_key": ""},
        }))
        .unwrap()
    }

    /// The catalog is read once, and again only when an installed text
    /// model is missing from it, as after a snap refresh brought new ones.
    #[test]
    fn the_catalog_is_read_again_only_for_a_model_it_lacks() {
        crate::ui::on_gtk_thread(|| {
            let reads = Rc::new(Cell::new(0));
            let section = ModelsSection::new(Deps {
                configurator: Rc::new(Snapd {
                    calls: Rc::default(),
                    answer: Ok(None),
                }),
                daemon: Rc::new(Daemon("idle")),
                overlay: adw::ToastOverlay::new(),
                follow_interval: Duration::from_millis(1),
                catalog: Box::new({
                    let reads = reads.clone();
                    move || {
                        reads.set(reads.get() + 1);
                        vec![catalog_entry()]
                    }
                }),
            });
            section.show(&found(false));
            section.show(&found(false));
            assert_eq!(reads.get(), 1, "everything installed is in it");
            let title = |section: &ModelsSection| {
                section
                    .rows
                    .borrow()
                    .last()
                    .unwrap()
                    .row
                    .title()
                    .to_string()
            };
            assert_eq!(title(&section), "English to Italiano");

            let mut snaps = snaps();
            snaps[1].components.push(SnapComponent {
                name: "model-translate-en-de".into(),
                summary: "English to German".into(),
                installed_size: 1,
                install_date: Some("2026-11-24T00:00:00Z".into()),
            });
            let speech = ["myna-parakeet".to_owned(), "myna-whisper".to_owned()];
            let lacking = Found {
                snaps: &snaps,
                speech: &speech,
                connected: Some("myna-parakeet"),
                busy: false,
            };
            section.show(&lacking);
            assert_eq!(reads.get(), 2, "a model it lacks reads it again");
            section.show(&lacking);
            assert_eq!(reads.get(), 3, "and again while it still lacks it");
        });
    }

    #[test]
    fn shutting_down_stops_following_a_removal() {
        crate::ui::on_gtk_thread(|| {
            let section = idle_section();
            assert!(!section.stopped());
            section.shutdown();
            assert!(section.stopped());
        });
    }

    #[test]
    fn the_add_rows_hand_over_to_their_callers() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("idle", Ok(Some("9".into())));
            let speech = Rc::new(Cell::new(false));
            let text = Rc::new(Cell::new(false));
            rig.section.connect_add_speech({
                let speech = speech.clone();
                move || speech.set(true)
            });
            rig.section.connect_add_text({
                let text = text.clone();
                move || text.set(true)
            });
            assert_eq!(rig.section.add_speech.title(), "Add speech model…");
            assert_eq!(rig.section.add_text.title(), "Add text model…");
            assert!(rig.section.add_speech.is_activatable());
            rig.section.add_speech.emit_by_name::<()>("activated", &[]);
            assert!(speech.get() && !text.get());
            rig.section.add_text.emit_by_name::<()>("activated", &[]);
            assert!(text.get());
            rig.section.add_text_unavailable();
            assert!(!rig.section.add_text.is_visible());
        });
    }
}
