//! The Steps group of the Processing tab: one row per step, a Translate row
//! with its target, the example line, and the installs a missing model needs.
//! The decisions are `processing_steps`; this binds them to widgets.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;

use crate::adapters::client_settings::GioClientSettings;
use crate::command::CancellationToken;
use crate::markup::escape_markup;
use crate::plugin_catalog::{self, Entry};
use crate::plugin_install::{self, Failure, SnapState};
use crate::ports::{
    BackendRepository, ClientSettingsSubscription, DictationDaemon, SystemConfigurator,
};
use crate::processing_steps::{
    example, rows, with_enabled, with_script_mode, with_script_on, with_target, Inputs, Offered,
    Row, Source, Standing, SCRIPT, TRANSLATE,
};
use crate::ui::InstallControl;

/// How long a finished install may wait for the daemon to offer its model
/// before the row offers it again.
const PREPARING_FOR: Duration = Duration::from_secs(30);

/// How often the daemon is asked what it offers.
const PAGE_KEY: &str = "myna-steps-page";

const REFRESH_EVERY: Duration = Duration::from_secs(5);

pub struct Deps {
    pub store: Rc<GioClientSettings>,
    pub daemon: Rc<dyn DictationDaemon>,
    pub configurator: Rc<dyn SystemConfigurator>,
    pub repository: Rc<dyn BackendRepository>,
    pub catalog: Box<dyn Fn() -> Vec<Entry>>,
    pub locales: Vec<String>,
    pub overlay: adw::ToastOverlay,
    /// How often an install's change is read for its progress.
    pub follow_interval: Duration,
    /// How often the daemon is asked what it offers.
    pub refresh_every: Duration,
}

impl Deps {
    pub fn refresh_default() -> Duration {
        REFRESH_EVERY
    }
}

enum Widgets {
    Switch {
        row: adw::SwitchRow,
        control: InstallControl,
    },
    Translate {
        row: adw::ExpanderRow,
        combo: adw::ComboRow,
        from: adw::ActionRow,
        control: InstallControl,
        /// The names the combo lists now, to refill it only when they change.
        listed: RefCell<Vec<String>>,
    },
    Script {
        row: adw::ExpanderRow,
        combo: adw::ComboRow,
    },
}

impl Widgets {
    fn widget(&self) -> gtk::Widget {
        match self {
            Self::Switch { row, .. } => row.clone().upcast(),
            Self::Translate { row, .. } => row.clone().upcast(),
            Self::Script { row, .. } => row.clone().upcast(),
        }
    }
}

pub struct StepsPage {
    deps: Deps,
    group: glib::WeakRef<adw::PreferencesGroup>,
    example: adw::ActionRow,
    widgets: RefCell<Vec<(String, Widgets)>>,
    shown: RefCell<Vec<Row>>,
    catalog: RefCell<Vec<Entry>>,
    offered: RefCell<Option<Offered>>,
    installing: RefCell<BTreeMap<String, Option<u8>>>,
    preparing: RefCell<BTreeMap<String, Instant>>,
    /// Set while the page writes a value into a widget, so the widget's
    /// signal is not taken for the user.
    updating: Cell<bool>,
    cancellation: CancellationToken,
    watch: RefCell<Option<Box<dyn ClientSettingsSubscription>>>,
}

impl Drop for StepsPage {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl StepsPage {
    /// Fill `group` with the steps and keep it current for as long as the
    /// group lives: the group owns the page, the page only watches the group.
    pub fn attach(deps: Deps, group: &adw::PreferencesGroup) -> Rc<Self> {
        let example = adw::ActionRow::builder()
            .title(gettextrs::gettext("For example"))
            .subtitle_lines(0)
            .css_classes(["property"])
            .build();
        let catalog = (deps.catalog)();
        let page = Rc::new(Self {
            deps,
            group: group.downgrade(),
            example,
            widgets: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            catalog: RefCell::new(catalog),
            offered: RefCell::new(None),
            installing: RefCell::new(BTreeMap::new()),
            preparing: RefCell::new(BTreeMap::new()),
            updating: Cell::new(false),
            cancellation: CancellationToken::new(),
            watch: RefCell::new(None),
        });
        let weak = Rc::downgrade(&page);
        page.watch
            .replace(Some(page.deps.store.watch_steps(move || {
                if let Some(page) = weak.upgrade() {
                    page.render();
                }
            })));
        page.render();
        // SAFETY: the key is private to this module and always holds a page.
        unsafe { group.set_data(PAGE_KEY, page.clone()) };
        let weak = Rc::downgrade(&page);
        let every = page.deps.refresh_every;
        glib::spawn_future_local(async move {
            loop {
                let Some(page) = weak.upgrade() else { return };
                page.refresh().await;
                drop(page);
                glib::timeout_future(every).await;
            }
        });
        page
    }

    /// Read what the daemon offers and the catalogs again, and redraw.
    pub async fn refresh(self: &Rc<Self>) {
        let offered = match self.deps.daemon.installed_steps().await {
            Ok(Some(json)) => Offered::parse(&json),
            _ => None,
        };
        self.offered.replace(offered);
        self.catalog.replace((self.deps.catalog)());
        self.render();
    }

    fn inputs_rows(&self, daemon: Option<&Offered>) -> Vec<Row> {
        let (ids, params) = self.deps.store.steps();
        let source = Source::of_setting(&self.deps.store.language());
        let catalog = self.catalog.borrow();
        let installing = self.installing.borrow();
        let now = Instant::now();
        let preparing: BTreeSet<String> = self
            .preparing
            .borrow()
            .iter()
            .filter(|(_, since)| now.duration_since(**since) < PREPARING_FOR)
            .map(|(id, _)| id.clone())
            .collect();
        rows(&Inputs {
            language_unset: self.deps.store.language().trim().is_empty(),
            enabled: &ids,
            params: &params,
            source: &source,
            catalog: &catalog,
            daemon,
            installing: &installing,
            preparing: &preparing,
            locales: &self.deps.locales,
        })
    }

    fn render(self: &Rc<Self>) {
        let offered = self.offered.borrow().clone();
        let rows = self.inputs_rows(offered.as_ref());
        if *self.shown.borrow() == rows && !self.widgets.borrow().is_empty() {
            return;
        }
        let same_steps = {
            let widgets = self.widgets.borrow();
            widgets.len() == rows.len()
                && widgets
                    .iter()
                    .zip(&rows)
                    .all(|((id, _), row)| *id == row.id)
        };
        if !same_steps {
            self.rebuild(&rows);
        }
        self.updating.set(true);
        for ((_, widgets), row) in self.widgets.borrow().iter().zip(&rows) {
            update(widgets, row);
        }
        let shown = example(&rows);
        let mut text = format!(
            "\u{201c}{}\u{201d}\n\u{2192} \u{201c}{}\u{201d}",
            shown.heard, shown.typed
        );
        if let Some(note) = &shown.note {
            text.push_str(&format!("\n{note}"));
        }
        self.example.set_subtitle(&escape_markup(&text));
        self.example
            .update_property(&[gtk::accessible::Property::Description(&text)]);
        self.updating.set(false);
        self.shown.replace(rows);
    }

    fn rebuild(self: &Rc<Self>, rows: &[Row]) {
        let Some(group) = self.group.upgrade() else {
            return;
        };
        let built_before = !self.widgets.borrow().is_empty();
        for (_, widgets) in self.widgets.take() {
            group.remove(&widgets.widget());
        }
        if built_before {
            group.remove(&self.example);
        }
        let mut built = Vec::new();
        for row in rows {
            let widgets = if row.id == TRANSLATE {
                self.translate_widgets(&row.id)
            } else if row.id == SCRIPT {
                self.script_widgets(&row.id)
            } else {
                self.switch_widgets(&row.id)
            };
            group.add(&widgets.widget());
            built.push((row.id.clone(), widgets));
        }
        group.add(&self.example);
        self.widgets.replace(built);
    }

    fn control(self: &Rc<Self>, id: &str) -> InstallControl {
        let control = InstallControl::new();
        control.set_visible(false);
        control.button().connect_clicked({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.install_missing(&id);
                }
            }
        });
        control
    }

    fn switch_widgets(self: &Rc<Self>, id: &str) -> Widgets {
        let row = adw::SwitchRow::builder().subtitle_lines(0).build();
        let control = self.control(id);
        row.add_suffix(&control);
        row.connect_active_notify({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |row| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        page.set_enabled(&id, row.is_active());
                    }
                }
            }
        });
        Widgets::Switch { row, control }
    }

    fn translate_widgets(self: &Rc<Self>, id: &str) -> Widgets {
        let row = adw::ExpanderRow::builder()
            .show_enable_switch(true)
            .subtitle_lines(0)
            .build();
        let control = self.control(id);
        row.add_suffix(&control);
        let combo = adw::ComboRow::builder()
            .title(gettextrs::gettext("Translate to"))
            .enable_search(true)
            .build();
        combo.set_expression(Some(gtk::PropertyExpression::new(
            gtk::StringObject::static_type(),
            None::<&gtk::Expression>,
            "string",
        )));
        let from = adw::ActionRow::builder()
            .title(gettextrs::gettext("From"))
            .subtitle(gettextrs::gettext("Your dictation language"))
            .css_classes(["property"])
            .visible(false)
            .build();
        row.add_row(&combo);
        row.add_row(&from);
        row.connect_enable_expansion_notify({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |row| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        if row.enables_expansion() {
                            row.set_expanded(true);
                        }
                        page.set_enabled(&id, row.enables_expansion());
                    }
                }
            }
        });
        combo.connect_selected_notify({
            let page = Rc::downgrade(self);
            move |combo| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        page.choose_target(combo.selected());
                    }
                }
            }
        });
        Widgets::Translate {
            row,
            combo,
            from,
            control,
            listed: RefCell::new(Vec::new()),
        }
    }

    fn script_widgets(self: &Rc<Self>, id: &str) -> Widgets {
        let row = adw::ExpanderRow::builder()
            .show_enable_switch(true)
            .subtitle_lines(0)
            .build();
        let combo = adw::ComboRow::builder()
            .title(gettextrs::gettext("Write Chinese as"))
            .build();
        row.add_row(&combo);
        row.connect_enable_expansion_notify({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |row| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        if row.enables_expansion() {
                            row.set_expanded(true);
                        }
                        page.set_enabled(&id, row.enables_expansion());
                    }
                }
            }
        });
        combo.connect_selected_notify({
            let page = Rc::downgrade(self);
            move |combo| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        page.choose_script(combo.selected());
                    }
                }
            }
        });
        Widgets::Script { row, combo }
    }

    fn write(&self, ids: &[String], params: &myna_core::processing::Params) {
        if let Err(error) = self.deps.store.set_steps(ids, params) {
            glib::g_warning!(crate::LOG_DOMAIN, "steps not stored: {error}");
            self.deps
                .overlay
                .add_toast(adw::Toast::new(&gettextrs::gettext(
                    "The change could not be saved.",
                )));
        }
    }

    fn set_enabled(self: &Rc<Self>, id: &str, on: bool) {
        let (ids, mut params) = self.deps.store.steps();
        if on && id == SCRIPT {
            params = with_script_on(&params, &self.deps.locales);
        }
        self.write(&with_enabled(&ids, id, on), &params);
        self.render();
    }

    fn choose_script(self: &Rc<Self>, index: u32) {
        let nick = {
            let shown = self.shown.borrow();
            shown
                .iter()
                .find(|row| row.id == SCRIPT)
                .and_then(|row| row.script.as_ref())
                .and_then(|script| script.modes.get(index as usize))
                .map(|(nick, _)| nick.clone())
        };
        let Some(nick) = nick else {
            return;
        };
        let (ids, params) = self.deps.store.steps();
        self.write(&ids, &with_script_mode(&params, &nick));
        self.render();
    }

    fn choose_target(self: &Rc<Self>, index: u32) {
        let target = {
            let shown = self.shown.borrow();
            let Some(translate) = shown
                .iter()
                .find(|row| row.id == TRANSLATE)
                .and_then(|row| row.translate.as_ref())
            else {
                return;
            };
            index
                .checked_sub(1)
                .and_then(|i| translate.targets.get(i as usize))
                .map(|target| (target.code.clone(), target.installed))
        };
        let (ids, params) = self.deps.store.steps();
        self.write(
            &ids,
            &with_target(&params, target.as_ref().map(|(code, _)| code.as_str())),
        );
        self.render();
        if matches!(target, Some((_, false))) {
            self.install_missing(TRANSLATE);
        }
    }

    /// Install what the enabled step `id` lacks.
    fn install_missing(self: &Rc<Self>, id: &str) {
        let offered = self.offered.borrow().clone().unwrap_or_default();
        let need = self
            .inputs_rows(Some(&offered))
            .into_iter()
            .find(|row| row.id == id)
            .and_then(|row| match row.standing {
                Standing::Missing(need) => Some(need),
                _ => None,
            });
        let Some(need) = need else { return };
        if self.installing.borrow().contains_key(id) {
            return;
        }
        self.installing.borrow_mut().insert(id.to_owned(), None);
        self.preparing.borrow_mut().remove(id);
        self.render();
        let refs: Vec<&Entry> = need.entries.iter().collect();
        let requests = plugin_catalog::requests(&refs);
        let page = Rc::downgrade(self);
        let id = id.to_owned();
        glib::spawn_future_local(async move {
            let outcome = run_install(&page, &id, requests).await;
            let Some(page) = page.upgrade() else { return };
            page.installing.borrow_mut().remove(&id);
            match outcome {
                Ok(()) => {
                    page.preparing.borrow_mut().insert(id, Instant::now());
                }
                Err(Failure::Dismissed) => {}
                Err(Failure::Dictating) => {
                    page.deps
                        .overlay
                        .add_toast(adw::Toast::new(&gettextrs::gettext(
                            "Finish dictating, then try again.",
                        )));
                }
                Err(Failure::Failed(error)) => page.toast_failure(&error),
            }
            page.refresh().await;
        });
    }

    fn toast_failure(&self, error: &crate::ports::SystemConfiguratorError) {
        let heading = gettextrs::gettext("Installing the model failed");
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

    async fn snap_state(&self, snap: &str) -> SnapState {
        let cancellation = self.cancellation.clone();
        let installed = self
            .deps
            .repository
            .installed_snaps(cancellation.clone())
            .await
            .map(|snaps| snaps.iter().any(|s| s.name == snap))
            .unwrap_or(false);
        let connected = self
            .deps
            .repository
            .discover(cancellation)
            .await
            .map(|snapshot| {
                snapshot
                    .text_offers()
                    .iter()
                    .any(|offer| offer.identity().snap_name() == snap && offer.connected())
            })
            .unwrap_or(false);
        SnapState {
            installed,
            connected,
        }
    }
}

/// Install each request in turn; the row shows the percentage of the one
/// running.
async fn run_install(
    page: &std::rc::Weak<StepsPage>,
    id: &str,
    requests: Vec<plugin_catalog::ComponentRequest>,
) -> Result<(), Failure> {
    for request in requests {
        let Some(strong) = page.upgrade() else {
            return Err(Failure::Dismissed);
        };
        let state = strong.snap_state(&request.snap).await;
        let report = {
            let page = page.clone();
            let id = id.to_owned();
            move |percent: Option<u8>| {
                if let Some(page) = page.upgrade() {
                    page.installing.borrow_mut().insert(id.clone(), percent);
                    page.render();
                }
            }
        };
        let sleep =
            |interval: Duration| -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>> {
                Box::pin(glib::timeout_future(interval))
            };
        let follow = crate::snap_install::Follow {
            interval: strong.deps.follow_interval,
            sleep: &sleep,
            cancellation: strong.cancellation.clone(),
        };
        let ctx = plugin_install::Context {
            configurator: strong.deps.configurator.as_ref(),
            daemon: strong.deps.daemon.as_ref(),
            follow: &follow,
        };
        plugin_install::install(&ctx, &request, state, &report).await?;
    }
    Ok(())
}

fn update(widgets: &Widgets, row: &Row) {
    let subtitle = escape_markup(&row.subtitle());
    match widgets {
        Widgets::Switch {
            row: widget,
            control,
        } => {
            widget.set_title(&escape_markup(&row.title));
            widget.set_subtitle(&subtitle);
            widget.set_active(row.enabled);
            widget.set_sensitive(row.switchable());
            show_standing(control, row);
        }
        Widgets::Translate {
            row: widget,
            combo,
            from,
            control,
            listed,
        } => {
            widget.set_title(&escape_markup(&row.title));
            widget.set_subtitle(&subtitle);
            widget.set_enable_expansion(row.enabled);
            widget.set_sensitive(row.switchable());
            show_standing(control, row);
            let Some(translate) = &row.translate else {
                return;
            };
            let names: Vec<String> = std::iter::once(gettextrs::gettext("Choose a language"))
                .chain(translate.targets.iter().map(|target| {
                    if target.installed {
                        target.name.clone()
                    } else {
                        // TRANSLATORS: {language} is a language name; its model is not installed yet.
                        let frame = gettextrs::gettext("{language} (not installed)");
                        frame.replace("{language}", &target.name)
                    }
                }))
                .collect();
            if *listed.borrow() != names {
                let refs: Vec<&str> = names.iter().map(String::as_str).collect();
                combo.set_model(Some(&gtk::StringList::new(&refs)));
                listed.replace(names);
            }
            combo.set_selected(translate.selected.map_or(0, |i| i as u32 + 1));
            combo.set_sensitive(row.enabled);
            from.set_visible(translate.from.is_some());
            if let Some(name) = &translate.from {
                from.set_subtitle(&escape_markup(name));
            }
        }
        Widgets::Script { row: widget, combo } => {
            widget.set_title(&escape_markup(&row.title));
            widget.set_subtitle(&subtitle);
            widget.set_enable_expansion(row.enabled);
            widget.set_sensitive(row.switchable());
            let Some(script) = &row.script else {
                return;
            };
            if combo.model().is_none() {
                let names: Vec<&str> = script.modes.iter().map(|(_, n)| n.as_str()).collect();
                combo.set_model(Some(&gtk::StringList::new(&names)));
            }
            combo.set_selected(script.selected as u32);
            combo.set_sensitive(row.enabled);
        }
    }
}

fn show_standing(control: &InstallControl, row: &Row) {
    control.set_visible(true);
    match &row.standing {
        Standing::Missing(need) => {
            // TRANSLATORS: {what} names a model, such as "the French model".
            let frame = gettextrs::gettext("Install {what}");
            control.show_offer(
                &gettextrs::gettext("Install"),
                &frame.replace("{what}", &need.what),
            );
        }
        Standing::Installing(percent) => control.show_installing(*percent),
        Standing::Preparing => {
            control.show_installing(None);
            control
                .progress()
                .label
                .set_label(&gettextrs::gettext("Preparing…"));
        }
        _ => control.set_visible(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::active_backend::SwitchPlan;
    use crate::backend_apply::ApplyPreview;
    use crate::domain::{
        ActiveBackendState, BackendIdentity, BackendSnapshot, BackendSurfaceError, CommandResult,
        ConnectionSnapshot,
    };
    use crate::ports::{
        PluginSetUp, SetUpPlan, SystemConfiguratorError, SystemConfiguratorFailure,
    };
    use crate::snap_changes::{parse_change, ChangeInProgress};
    use async_trait::async_trait;
    use serde_json::json;

    const TIDY: &str = r#"{"models":[{"model":"m-tidy","task":"text.cleanup"}],"steps":[]}"#;
    const FRENCH: &str = r#"{"models":[{"model":"m-fr","task":"text.translate","params":[{"name":"language","values":["en"]},{"name":"to","values":["fr"]}]}],"steps":[]}"#;
    const NOTHING: &str = r#"{"models":[],"steps":[]}"#;

    struct Daemon(Rc<RefCell<String>>);

    #[async_trait(?Send)]
    impl DictationDaemon for Daemon {
        async fn state(&self) -> Result<Option<String>, String> {
            Ok(Some("idle".into()))
        }
        async fn offered_models(&self) -> Result<Option<Vec<String>>, String> {
            Ok(Some(Vec::new()))
        }
        async fn installed_steps(&self) -> Result<Option<String>, String> {
            Ok(Some(self.0.borrow().clone()))
        }
        async fn rescan(&self) -> Result<(), String> {
            Ok(())
        }
    }

    struct Snapd {
        calls: Rc<RefCell<Vec<String>>>,
        daemon: Rc<RefCell<String>>,
        /// What the daemon offers once the install is done.
        after: &'static str,
    }

    struct Nap(bool);
    impl std::future::Future for Nap {
        type Output = ();
        fn poll(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<()> {
            if self.0 {
                return std::task::Poll::Ready(());
            }
            self.0 = true;
            let waker = cx.waker().clone();
            glib::timeout_add_local_once(Duration::from_millis(30), move || waker.wake());
            std::task::Poll::Pending
        }
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
            unreachable!()
        }
        async fn set_up_plugin(
            &self,
            plan: &PluginSetUp,
            _: CancellationToken,
        ) -> Result<(), SystemConfiguratorError> {
            self.calls
                .borrow_mut()
                .push(format!("{} {}", plan.snap, plan.components.join("+")));
            for _ in 0..8 {
                Nap(false).await;
            }
            self.daemon.replace(self.after.to_owned());
            Ok(())
        }
        async fn changes_in_progress(
            &self,
            _: CancellationToken,
        ) -> Result<Vec<ChangeInProgress>, String> {
            Ok(vec![parse_change(json!({
                "id": "7", "kind": "install-component", "ready": false, "status": "Doing",
                "summary": "Install component \"model-x\" for snap \"myna-translation\"",
                "tasks": [{"kind": "download-component", "status": "Doing",
                    "progress": {"label": "x", "done": 50, "total": 100}}],
            }))
            .unwrap()])
        }
        async fn snap_change(
            &self,
            _: &str,
            _: CancellationToken,
        ) -> Result<ChangeInProgress, String> {
            unreachable!()
        }
    }

    struct Repository;

    #[async_trait(?Send)]
    impl BackendRepository for Repository {
        async fn discover(
            &self,
            _: CancellationToken,
        ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
            Ok(ConnectionSnapshot::new(
                Vec::new(),
                ActiveBackendState::Disconnected,
            ))
        }
        async fn read_snapshot(
            &self,
            backend: &BackendIdentity,
            _: CancellationToken,
        ) -> BackendSnapshot {
            BackendSnapshot::empty(backend.clone())
        }
        async fn refresh(
            &self,
            _: CancellationToken,
        ) -> Result<ConnectionSnapshot, BackendSurfaceError> {
            self.discover(CancellationToken::new()).await
        }
    }

    fn entries() -> Vec<Entry> {
        let one = |id: &str, step: &str, stage: &str, from: &str, to: &str, bytes: u64| {
            serde_json::from_value::<Entry>(json!({
                "id": id, "snap": "myna-translation", "component": id, "model": format!("m-{id}"),
                "task": "text.translate", "in_lang": from, "out_lang": to, "download_bytes": bytes,
                "step": {"id": step, "stage": stage, "label_key": format!("step.{step}.name"),
                         "description_key": format!("step.{step}.description"),
                         "example_key": format!("step.{step}.example")},
            }))
            .unwrap()
        };
        vec![
            one("tidy", "tidy", "clean-up", "en", "en", 42_000_000),
            one("fr", "translate", "translate", "en", "fr", 33_000_000),
            one("de", "translate", "translate", "en", "de", 31_000_000),
        ]
    }

    struct Rig {
        page: Rc<StepsPage>,
        group: adw::PreferencesGroup,
        store: Rc<GioClientSettings>,
        daemon: Rc<RefCell<String>>,
        calls: Rc<RefCell<Vec<String>>>,
        _dir: std::path::PathBuf,
    }

    fn rig(name: &str, daemon: &str, after: &'static str, keyfile: &str) -> Rig {
        crate::ui::register_resources();
        let _ = adw::init();
        let dir = std::env::temp_dir().join(format!("myna-steps-{name}-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("schemas")).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../data/glib-2.0/schemas/com.canonical.Myna.Dictation.gschema.xml"),
            dir.join("schemas/com.canonical.Myna.Dictation.gschema.xml"),
        )
        .unwrap();
        assert!(std::process::Command::new("glib-compile-schemas")
            .arg(dir.join("schemas"))
            .status()
            .unwrap()
            .success());
        std::fs::write(
            dir.join("keyfile"),
            format!("[com/canonical/myna/dictation]\n{keyfile}"),
        )
        .unwrap();
        let source =
            gio::SettingsSchemaSource::from_directory(dir.join("schemas"), None, false).unwrap();
        let store =
            Rc::new(GioClientSettings::open_with_source(&source, dir.join("keyfile")).unwrap());
        let daemon = Rc::new(RefCell::new(daemon.to_owned()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let group = adw::PreferencesGroup::new();
        let page = StepsPage::attach(
            Deps {
                store: store.clone(),
                daemon: Rc::new(Daemon(daemon.clone())),
                configurator: Rc::new(Snapd {
                    calls: calls.clone(),
                    daemon: daemon.clone(),
                    after,
                }),
                repository: Rc::new(Repository),
                catalog: Box::new(entries),
                locales: vec!["fr_FR.UTF-8".into()],
                overlay: adw::ToastOverlay::new(),
                follow_interval: Duration::from_millis(20),
                refresh_every: Duration::from_millis(60),
            },
            &group,
        );
        Rig {
            page,
            group,
            store,
            daemon,
            calls,
            _dir: dir,
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

    fn all<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
        let mut found = Vec::new();
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Ok(wanted) = widget.clone().downcast::<T>() {
                found.push(wanted);
            }
            found.extend(all::<T>(&widget));
            child = widget.next_sibling();
        }
        found
    }

    fn switch(rig: &Rig, title: &str) -> adw::SwitchRow {
        all::<adw::SwitchRow>(rig.group.upcast_ref())
            .into_iter()
            .find(|row| row.title() == title)
            .unwrap_or_else(|| panic!("no switch row {title}"))
    }

    fn expander(rig: &Rig) -> adw::ExpanderRow {
        all::<adw::ExpanderRow>(rig.group.upcast_ref()).remove(0)
    }

    fn combo(rig: &Rig) -> adw::ComboRow {
        all::<adw::ComboRow>(rig.group.upcast_ref()).remove(0)
    }

    fn controls(rig: &Rig) -> Vec<InstallControl> {
        all::<InstallControl>(rig.group.upcast_ref())
    }

    fn visible_button(rig: &Rig) -> Option<InstallControl> {
        controls(rig)
            .into_iter()
            .find(|c| c.is_visible() && c.button().is_visible())
    }

    #[test]
    fn the_rows_are_the_catalogs_steps_in_stage_order_with_the_example() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("order", NOTHING, NOTHING, "");
            let titles: Vec<String> = all::<adw::PreferencesRow>(rig.group.upcast_ref())
                .iter()
                .map(|row| row.title().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            let at = |t: &str| titles.iter().position(|x| x == t).unwrap();
            assert!(at("Fitted to the field") < at("Tidy up speech"));
            assert!(at("Tidy up speech") < at("Translate"));
            assert_eq!(titles.last().unwrap(), "For example");
            assert!(!switch(&rig, "Tidy up speech").is_active());
        });
    }

    #[test]
    fn a_switch_writes_the_step_and_the_example_follows() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("switch", TIDY, TIDY, "");
            let tidy = switch(&rig, "Tidy up speech");
            tidy.set_active(true);
            assert_eq!(rig.store.steps().0, ["tidy"]);
            pump_until("the example", || {
                rig.page
                    .example
                    .subtitle()
                    .is_some_and(|s| s.contains("So I think we should go."))
            });
            tidy.set_active(false);
            assert!(rig.store.steps().0.is_empty());
        });
    }

    #[test]
    fn a_step_that_cannot_apply_is_dimmed_with_its_reason() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "dim",
                TIDY,
                TIDY,
                "language='fr'\nprocessing-steps=['tidy']\n",
            );
            let tidy = switch(&rig, "Tidy up speech");
            assert!(!tidy.is_sensitive());
            assert_eq!(tidy.subtitle().unwrap(), "Applies when the text is English");
            let several = self::rig("several", TIDY, TIDY, "language='en,fr'\n");
            assert!(!expander(&several).is_sensitive());
            assert_eq!(
                expander(&several).subtitle(),
                "Needs a single dictation language"
            );
        });
    }

    #[test]
    fn an_enabled_step_without_its_model_offers_an_install_that_runs() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("install", NOTHING, TIDY, "processing-steps=['tidy']\n");
            let tidy = switch(&rig, "Tidy up speech");
            pump_until("the daemon's answer", || {
                tidy.subtitle()
                    .is_some_and(|s| s.starts_with("Needs the Tidy up speech model"))
            });
            let control = visible_button(&rig).expect("an Install button");
            assert_eq!(control.button().label().as_deref(), Some("Install"));
            control.button().emit_clicked();
            pump_until("the install to show", || {
                controls(&rig)
                    .iter()
                    .any(|c| c.progress().container.is_visible())
            });
            pump_until("the request", || !rig.calls.borrow().is_empty());
            assert_eq!(rig.calls.borrow().as_slice(), ["myna-translation tidy"]);
            pump_until("the row to be ready", || {
                visible_button(&rig).is_none()
                    && controls(&rig).iter().all(|c| !c.is_visible())
                    && !tidy.subtitle().unwrap().starts_with("Needs")
            });
            assert!(rig.daemon.borrow().contains("m-tidy"));
        });
    }

    #[test]
    fn choosing_an_available_language_starts_its_install_and_the_row_prepares() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "choose",
                NOTHING,
                FRENCH,
                "processing-steps=['translate']\n",
            );
            let combo = combo(&rig);
            let listed: Vec<String> = (0..combo.model().unwrap().n_items())
                .filter_map(|i| combo.model()?.item(i)?.downcast::<gtk::StringObject>().ok())
                .map(|s| s.string().to_string())
                .collect();
            assert_eq!(
                listed,
                [
                    "Choose a language",
                    "Français (not installed)",
                    "Deutsch (not installed)"
                ]
            );
            combo.set_selected(1);
            assert_eq!(rig.store.steps().1["translate.to"], "fr");
            pump_until("the install to show", || {
                controls(&rig)
                    .iter()
                    .any(|c| c.progress().container.is_visible())
            });
            pump_until("the request", || !rig.calls.borrow().is_empty());
            assert_eq!(rig.calls.borrow().as_slice(), ["myna-translation fr"]);
            pump_until("French to be installed", || {
                combo
                    .model()
                    .unwrap()
                    .item(1)
                    .and_then(|i| i.downcast::<gtk::StringObject>().ok())
                    .is_some_and(|s| s.string() == "Français")
            });
        });
    }

    #[test]
    fn the_dictation_language_is_shown_when_it_is_not_english() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "from",
                NOTHING,
                NOTHING,
                "language='de'\nprocessing-steps=['translate']\n",
            );
            let from = all::<adw::ActionRow>(rig.group.upcast_ref())
                .into_iter()
                .find(|row| row.title() == "From")
                .unwrap();
            assert!(from.is_visible());
            assert_eq!(from.subtitle().unwrap(), "Deutsch");
        });
    }

    #[test]
    fn the_group_keeps_the_page_alive_without_anyone_holding_it() {
        crate::ui::on_gtk_thread(|| {
            let Rig {
                page, group, store, ..
            } = rig("owned", TIDY, TIDY, "");
            drop(page);
            let tidy = all::<adw::SwitchRow>(group.upcast_ref())
                .into_iter()
                .find(|row| row.title() == "Tidy up speech")
                .unwrap();
            tidy.set_active(true);
            assert_eq!(store.steps().0, ["tidy"]);
        });
    }

    #[test]
    fn a_change_to_the_store_moves_the_rows() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("watch", TIDY, TIDY, "");
            rig.store
                .set_steps(&["fit".to_owned()], &Default::default())
                .unwrap();
            pump_until("the switch", || {
                switch(&rig, "Fitted to the field").is_active()
            });
        });
    }

    fn script_row(rig: &Rig) -> (adw::ExpanderRow, adw::ComboRow) {
        let row = all::<adw::ExpanderRow>(rig.group.upcast_ref())
            .into_iter()
            .find(|row| row.title() == "Chinese script")
            .expect("the Chinese script row");
        let combo = all::<adw::ComboRow>(row.upcast_ref())
            .into_iter()
            .next()
            .expect("its mode combo");
        (row, combo)
    }

    #[test]
    fn the_chinese_script_row_is_dimmed_for_english_with_no_snap_installed() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("script-dim", NOTHING, NOTHING, "");
            let (row, _) = script_row(&rig);
            assert!(!row.is_sensitive());
            assert_eq!(row.subtitle(), "Applies when the text is Chinese");
            assert!(!row.enables_expansion());
        });
    }

    #[test]
    fn switching_chinese_script_on_stores_a_mode_and_the_combo_changes_it() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("script-on", NOTHING, NOTHING, "language='zh'\n");
            let (row, combo) = script_row(&rig);
            assert!(row.is_sensitive());
            assert!(row.subtitle().starts_with("Types Chinese"));
            assert!(!combo.is_sensitive());
            row.set_enable_expansion(true);
            let (ids, params) = rig.store.steps();
            assert_eq!(ids, ["script"]);
            assert_eq!(params["script.mode"], "simplified");
            assert!(combo.is_sensitive());
            let shown: Vec<String> = (0..4)
                .map(|i| {
                    combo
                        .model()
                        .unwrap()
                        .item(i)
                        .and_downcast::<gtk::StringObject>()
                        .unwrap()
                        .string()
                        .to_string()
                })
                .collect();
            assert_eq!(shown[2], "Traditional, Taiwan (繁體)");
            combo.set_selected(2);
            assert_eq!(rig.store.steps().1["script.mode"], "traditional-taiwan");
            assert_eq!(rig.store.steps().0, ["script"]);
            row.set_enable_expansion(false);
            assert!(rig.store.steps().0.is_empty());
            assert_eq!(
                rig.store.steps().1["script.mode"],
                "traditional-taiwan",
                "the choice outlives the switch"
            );
        });
    }

    #[test]
    fn only_switching_the_script_on_stores_a_mode() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "script-mode-only",
                NOTHING,
                TIDY,
                "language='zh'\nprocessing-steps=['script']\n",
            );
            let (row, _) = script_row(&rig);
            row.set_enable_expansion(false);
            assert!(rig.store.steps().0.is_empty());
            assert!(rig.store.steps().1.is_empty(), "off stores no mode");
            let tidy = switch(&rig, "Tidy up speech");
            tidy.set_active(true);
            assert!(rig.store.steps().1.is_empty(), "another step stores none");
        });
    }

    #[test]
    fn a_chinese_users_stored_choice_shows_on_the_row() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "script-stored",
                NOTHING,
                NOTHING,
                "language='zh-Hant'\nprocessing-steps=['script']\n\
                 processing-params={'script.mode': 'traditional-hongkong'}\n",
            );
            let (row, combo) = script_row(&rig);
            assert!(row.enables_expansion());
            assert_eq!(combo.selected(), 3);
        });
    }
}
