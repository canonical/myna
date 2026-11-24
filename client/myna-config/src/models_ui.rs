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
    row: adw::ActionRow,
    remove: Option<gtk::Button>,
    spinner: gtk::Spinner,
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
            self.rebuild(&models);
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

    fn rebuild(self: &Rc<Self>, models: &Models) {
        for model in self.rows.take() {
            self.speech_group.remove(&model.row);
            self.text_group.remove(&model.row);
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
                let built = self.model_row(model);
                group.add(&built.row);
                rows.push(built);
            }
        }
        self.speech_group.add(&self.add_speech);
        self.text_group.add(&self.add_text);
        self.rows.replace(rows);
    }

    fn model_row(self: &Rc<Self>, model: &Model) -> ModelRow {
        // TRANSLATORS: {kind} says what a model is, such as "Translation" or "Speech recognition", and {size} its size on disk, such as "729 MB". The spaces around the dot are no-break spaces.
        let frame = gettextrs::gettext("{kind}\u{a0}·\u{a0}{size}");
        let subtitle = frame
            .replace("{kind}", &model.kind)
            .replace("{size}", &glib::format_size(model.bytes));
        let row = adw::ActionRow::builder()
            .title(escape_markup(&model.title))
            .subtitle(escape_markup(&subtitle))
            .build();
        row.update_property(&[gtk::accessible::Property::Description(&subtitle)]);
        let spinner = gtk::Spinner::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        row.add_suffix(&spinner);
        let remove = if model.in_use {
            let label = gtk::Label::builder()
                .label(gettextrs::gettext("In use"))
                .valign(gtk::Align::Center)
                .css_classes(["dim-label"])
                .build();
            row.add_suffix(&label);
            None
        } else {
            let button = gtk::Button::builder()
                .label(gettextrs::gettext("Remove"))
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            button.reset_relation(gtk::AccessibleRelation::LabelledBy);
            button.update_property(&[gtk::accessible::Property::Label(
                // TRANSLATORS: {model} names a model, such as "Whisper" or "English to Italiano".
                &gettextrs::gettext("Remove {model}").replace("{model}", &model.title),
            )]);
            button.connect_clicked({
                let section = Rc::downgrade(self);
                let id = model.id.clone();
                move |_| {
                    if let Some(section) = section.upgrade() {
                        section.start_remove(&id);
                    }
                }
            });
            row.add_suffix(&button);
            Some(button)
        };
        ModelRow {
            id: model.id.clone(),
            row,
            remove,
            spinner,
        }
    }

    fn blocked(&self) -> bool {
        self.busy.get() || self.removing.borrow().is_some()
    }

    fn render(&self) {
        let removing = self.removing.borrow().clone();
        let blocked = self.blocked();
        for model in self.rows.borrow().iter() {
            let this = removing.as_deref() == Some(model.id.as_str());
            model.spinner.set_visible(this);
            model.spinner.set_spinning(this);
            if let Some(button) = &model.remove {
                button.set_visible(!this);
                button.set_sensitive(!blocked);
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
        let summary = crate::backend_ui::system_failure_summary(error);
        let details = crate::backend_ui::system_error_details(error);
        let toast = adw::Toast::builder()
            .title(escape_markup(&heading))
            .button_label(gettextrs::gettext("Details"))
            .build();
        toast.connect_button_clicked({
            let overlay = self.deps.overlay.clone();
            move |_| {
                crate::ui::OperationErrorDialog::new(&heading, &summary, &details)
                    .present(Some(overlay.upcast_ref::<gtk::Widget>()));
            }
        });
        self.deps.overlay.add_toast(toast);
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
        (found.row.clone(), found.remove.clone())
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
            rig.section.add_speech.emit_by_name::<()>("activated", &[]);
            assert!(speech.get() && !text.get());
            rig.section.add_text.emit_by_name::<()>("activated", &[]);
            assert!(text.get());
            rig.section.add_text_unavailable();
            assert!(!rig.section.add_text.is_visible());
        });
    }
}
