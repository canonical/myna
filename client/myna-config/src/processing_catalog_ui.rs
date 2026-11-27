//! The catalog page the editor adds steps from, the language page Translate
//! opens, and the installs and removals both drive. The decisions are
//! `step_catalog`, `processing_steps` and `plugin_install`; this binds them to
//! widgets and to the page that owns them.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;
use myna_core::chains::{Chain, ChainStep, Processing};

use crate::chains_view;
use crate::languages::{language_in_text, Source};
use crate::markup::escape_markup;
use crate::plugin_catalog::{self, Entry};
use crate::plugin_install::{self, Failure, SnapState};
use crate::processing_steps::{self as steps, size, Row, Standing, TRANSLATE};
use crate::step_catalog::{self, Language, LanguageState};
use crate::ui::InstallControl;

use super::processing_step_ui::show_standing;
use super::processing_ui::{describe, name, StepsPage};

/// The expanded rows under a catalog item: its example, its size, its
/// quality and its licence, as the catalog schema has them.
pub(crate) fn catalog_details(row: &adw::ExpanderRow, item: &Row) {
    let example = item
        .example
        .iter()
        .map(|example| (gettextrs::gettext("For example"), example.clone()));
    for (label, text) in example.chain(item.details.clone()) {
        row.add_row(
            &adw::ActionRow::builder()
                .title(escape_markup(&label))
                .subtitle(escape_markup(&text))
                .subtitle_lines(0)
                .subtitle_selectable(true)
                .css_classes(["property"])
                .build(),
        );
    }
}

/// What the catalog page shows, and whether a change may start from it.
pub(crate) struct CatalogView {
    items: Vec<Row>,
    languages: Vec<Language>,
    /// The language the text is in where the Translate step would go.
    from: Source,
    /// The installed translation models the user may take away.
    removable: Vec<Entry>,
    /// The removals in flight, so a row whose model is going shows it.
    removing: BTreeSet<String>,
    blocked: bool,
}

pub(crate) fn language_page() -> LanguagePage {
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettextrs::gettext("Search languages"))
        .hexpand(true)
        .build();
    name(&search, &gettextrs::gettext("Search languages"));
    let list = adw::PreferencesGroup::new();
    let empty = adw::ActionRow::builder()
        .title(gettextrs::gettext("No language matches"))
        .subtitle(gettextrs::gettext(
            "Only languages a model can translate into from here are listed.",
        ))
        .subtitle_lines(0)
        .visible(false)
        .build();
    list.add(&empty);
    let installed = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Installed translation models"))
        .description(gettextrs::gettext(
            "A step that uses a removed model stays and shows what it needs, so it can be installed again.",
        ))
        .visible(false)
        .build();
    let preferences = adw::PreferencesPage::builder().vexpand(true).build();
    preferences.add(&list);
    preferences.add(&installed);
    let bar = adw::Clamp::builder()
        .maximum_size(600)
        .margin_top(12)
        .margin_start(12)
        .margin_end(12)
        .child(&search)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&bar);
    content.append(&preferences);
    let page = crate::subpage::subpage(&gettextrs::gettext("Translate to"), &content);
    LanguagePage {
        page,
        search,
        list,
        empty,
        rows: RefCell::new(Vec::new()),
        installed,
        installed_rows: RefCell::new(Vec::new()),
    }
}

pub(crate) fn show_language(widgets: &LanguageWidgets, language: &Language, blocked: bool) {
    widgets.row.set_title(&escape_markup(&language.name));
    widgets
        .row
        .set_subtitle(&escape_markup(language.english.as_deref().unwrap_or("")));
    let (state, sensitive) = match &language.state {
        LanguageState::Ready => (gettextrs::gettext("Installed"), true),
        LanguageState::Download { bytes, entries } => {
            let text = if entries.len() > 1 {
                // TRANSLATORS: {size} is a download size, such as "66 MB"; two models make the translation, through English.
                gettextrs::gettext("Downloads {size}, through English")
            } else {
                // TRANSLATORS: {size} is a download size, such as "33 MB".
                gettextrs::gettext("Downloads {size}")
            };
            (text.replace("{size}", &size(*bytes)), !blocked)
        }
        LanguageState::Installing(Some(percent)) => {
            // TRANSLATORS: {percent} is a number from 0 to 100.
            let frame = gettextrs::gettext("Installing, {percent}%");
            (frame.replace("{percent}", &percent.to_string()), false)
        }
        LanguageState::Installing(None) => (gettextrs::gettext("Installing…"), false),
        LanguageState::Preparing => (gettextrs::gettext("Preparing…"), false),
        LanguageState::Unknown => (gettextrs::gettext("Myna is not running"), true),
        // Only a language the editor already chose is listed this way.
        LanguageState::NoModel => (gettextrs::gettext("No model installed"), false),
    };
    widgets.state.set_label(&state);
    widgets.row.set_sensitive(sensitive);
    let description = match &language.english {
        Some(english) => format!("{english}. {state}"),
        None => state,
    };
    describe(&widgets.row, &description);
}

pub(crate) fn show_item(widgets: &CatalogWidgets, row: &Row, blocked: bool, removing: bool) {
    widgets.row.set_title(&escape_markup(&row.title));
    let subtitle = match &row.standing {
        Standing::NotApplicable(reason) => reason.clone(),
        Standing::Unknown => gettextrs::gettext("Myna is not running"),
        _ => row.description.clone(),
    };
    widgets.row.set_subtitle(&escape_markup(&subtitle));
    // Added once its model is there and the step is on; the Add button only
    // while the step is off, insensitive when the text here does not suit it.
    let installed = matches!(
        row.standing,
        Standing::Ready | Standing::NotApplicable(_) | Standing::Unknown
    );
    let added = row.on && installed;
    let add = !row.on && matches!(row.standing, Standing::Ready);
    widgets
        .add
        .set_visible(add || matches!(row.standing, Standing::NotApplicable(_)));
    widgets.add.set_sensitive(add);
    widgets.added.set_visible(added);
    widgets.remove_row.set_visible(row.removable.is_some());
    widgets.remove.set_sensitive(!blocked);
    let control = &widgets.control;
    control.set_visible(true);
    if removing {
        control.show_installing(None);
        control
            .progress()
            .label
            .set_label(&gettextrs::gettext("Removing…"));
    } else if matches!(row.standing, Standing::Missing(_)) {
        control.show_offer(
            &gettextrs::gettext("Install"),
            // TRANSLATORS: {step} is a step's name, such as "Translate English to French".
            &gettextrs::gettext("Install {step}").replace("{step}", &row.title),
        );
        control.button().set_sensitive(!blocked);
    } else {
        show_standing(control, &row.standing);
    }
}

/// Install each request in turn; the row shows the percentage of the one
/// running.
async fn run_install(
    page: &std::rc::Weak<StepsPage>,
    id: &str,
    requests: Vec<plugin_catalog::ComponentRequest>,
) -> Result<plugin_install::Outcome, Failure> {
    let mut outcome = plugin_install::Outcome::Settled;
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
        if plugin_install::install(&ctx, &request, state, &report).await?
            == plugin_install::Outcome::Unconfirmed
        {
            outcome = plugin_install::Outcome::Unconfirmed;
        }
    }
    Ok(outcome)
}

/// Remove each component of `entry`.
async fn run_remove(page: &std::rc::Weak<StepsPage>, entry: &Entry) -> Result<(), Failure> {
    for request in plugin_catalog::requests(&[entry]) {
        let Some(strong) = page.upgrade() else {
            return Err(Failure::Dismissed);
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
        plugin_install::remove(&ctx, &request, &|_| {}).await?;
    }
    Ok(())
}

/// The page Translate opens: a search over the languages the text can be
/// translated into, and the installed translation models to remove.
pub(crate) struct LanguagePage {
    pub(crate) page: adw::NavigationPage,
    pub(crate) search: gtk::SearchEntry,
    /// Says which language the text is in where the step would go.
    pub(crate) list: adw::PreferencesGroup,
    pub(crate) empty: adw::ActionRow,
    pub(crate) rows: RefCell<Vec<(String, LanguageWidgets)>>,
    pub(crate) installed: adw::PreferencesGroup,
    pub(crate) installed_rows: RefCell<Vec<(String, adw::ActionRow, gtk::Button)>>,
}

pub(crate) struct LanguageWidgets {
    pub(crate) row: adw::ActionRow,
    pub(crate) state: gtk::Label,
}

/// One item of the catalog page.
pub(crate) struct CatalogWidgets {
    pub(crate) row: adw::ExpanderRow,
    pub(crate) control: InstallControl,
    pub(crate) add: gtk::Button,
    pub(crate) added: gtk::Label,
    pub(crate) remove_row: adw::ActionRow,
    pub(crate) remove: gtk::Button,
}

impl StepsPage {
    /// The catalog for the active chain, opened from elsewhere in Settings
    /// (Diagnostics' Add text model): Install there only installs.
    pub fn open_catalog_to_add(self: &Rc<Self>) {
        self.open_catalog(None);
    }

    pub(crate) fn open_catalog(self: &Rc<Self>, chain: Option<&str>) {
        self.catalog_for.replace(chain.map(str::to_owned));
        self.render();
        self.deps.nav.push(&self.catalog_page);
    }

    /// The chain the catalog adds to now.
    pub(crate) fn catalog_chain(&self, processing: &Processing) -> Option<Chain> {
        match self.catalog_for.borrow().as_deref() {
            Some(id) => processing.chain(id).cloned(),
            None => processing.active().cloned(),
        }
    }

    /// Install `entries` as one operation, shown under `id` (a row's `key`).
    /// Only one runs at a time: the controls are disabled meanwhile.
    pub(crate) fn start_install(self: &Rc<Self>, id: &str, entries: &[Entry]) {
        if !self.installing.borrow().is_empty() || !self.removing.borrow().is_empty() {
            return;
        }
        self.installing.borrow_mut().insert(id.to_owned(), None);
        self.preparing.borrow_mut().remove(id);
        self.render();
        let refs: Vec<&Entry> = entries.iter().collect();
        let requests = plugin_catalog::requests(&refs);
        let page = Rc::downgrade(self);
        let id = id.to_owned();
        glib::spawn_future_local(async move {
            let outcome = run_install(&page, &id, requests).await;
            let Some(page) = page.upgrade() else { return };
            page.installing.borrow_mut().remove(&id);
            let unconfirmed = matches!(outcome, Ok(plugin_install::Outcome::Unconfirmed));
            if unconfirmed {
                page.preparing.borrow_mut().insert(id, Instant::now());
            }
            page.report_outcome(
                &outcome.map(|_| ()),
                &gettextrs::gettext("Installing the model failed"),
            );
            page.refresh().await;
            if unconfirmed {
                let weak = Rc::downgrade(&page);
                glib::timeout_add_local_once(crate::processing_ui::PREPARING_FOR, move || {
                    if let Some(page) = weak.upgrade() {
                        page.render();
                    }
                });
            }
        });
    }

    pub(crate) fn render_catalog(self: &Rc<Self>, processing: &Processing) {
        let view = self.catalog_view(processing);
        self.paint_catalog(&view);
        let (blocked, from, languages, removable) = (
            view.blocked,
            view.from.clone(),
            view.languages.clone(),
            view.removable.clone(),
        );
        self.items.replace(view.items);
        self.render_languages(&from, languages, &removable, blocked);
    }

    /// What the catalog page lists now, decided by `step_catalog` from what
    /// the daemon offers, what is installed and the chain it adds to.
    pub(crate) fn catalog_view(&self, processing: &Processing) -> CatalogView {
        let target = self.catalog_chain(processing);
        let offered = self.offered.borrow().clone();
        let source = Source::of_setting(&self.deps.store.language());
        let from = target.as_ref().map_or_else(
            || source.clone(),
            |chain| {
                let at = myna_core::chains::insert_position(&chain.steps, TRANSLATE);
                self.view(|ctx| chains_view::language_at(&chain.steps, at, ctx))
            },
        );
        let catalog = self.catalog.borrow();
        let installing = self.installing.borrow();
        let removing = self.removing.borrow();
        let chain_rows = target
            .as_ref()
            .map(|chain| self.view(|ctx| chains_view::lines(chain, ctx)))
            .unwrap_or_default();
        let likely = self.likely_languages(processing);
        let steps = target.as_ref().map(|c| c.steps.clone()).unwrap_or_default();
        let at = myna_core::chains::insert_position(&steps, TRANSLATE);
        let strands = |code: &str| {
            let mut after = steps.clone();
            after.insert(at, ChainStep::new(TRANSLATE).with("to", code));
            self.view(|ctx| chains_view::strands(&steps, &after, ctx))
        };
        let mut language_installs = BTreeMap::new();
        if let (Some(code), Source::One(from_code)) =
            (self.installing_language.borrow().as_ref(), &from)
        {
            if let Some(percent) = installing.get(&steps::translate_key(from_code, code)) {
                language_installs.insert(step_catalog::language_key(code), *percent);
            }
        }
        let languages = step_catalog::languages(&step_catalog::LanguageInputs {
            catalog: &catalog,
            daemon: offered.as_ref(),
            from: &from,
            installing: &language_installs,
            preparing: &BTreeSet::new(),
            likely: &likely,
            strands: &strands,
            chosen: None,
        });
        let items = step_catalog::rows(&step_catalog::Inputs {
            catalog: &catalog,
            rows: &chain_rows,
            languages: &languages,
            translate_from: &from,
            source: &source,
            language_unset: self.deps.store.language().trim().is_empty(),
            daemon: offered.as_ref(),
            installing: &installing,
            preparing: &self.preparing_now(),
            removing: &removing,
            locales: &self.deps.locales,
        });
        let removable: Vec<Entry> = catalog
            .iter()
            .filter(|e| e.step.id == TRANSLATE)
            .filter(|e| offered.as_ref().is_some_and(|d| d.offers(&e.model)))
            .cloned()
            .collect();
        CatalogView {
            blocked: self.dictating.get() || !installing.is_empty() || !removing.is_empty(),
            items,
            languages,
            from,
            removable,
            removing: removing.clone(),
        }
    }

    /// Build the catalog's rows when the list changed, then paint them.
    pub(crate) fn paint_catalog(self: &Rc<Self>, view: &CatalogView) {
        let items = &view.items;
        let same = {
            let widgets = self.catalog_widgets.borrow();
            widgets.len() == items.len()
                && widgets
                    .iter()
                    .zip(items)
                    .all(|((id, _), row)| *id == row.id)
        };
        if !same {
            for (_, widgets) in self.catalog_widgets.take() {
                match widgets {
                    Some(widgets) => self.catalog_group.remove(&widgets.row),
                    None => self.catalog_group.remove(&self.translate_row),
                }
            }
            let mut built = Vec::new();
            for row in items {
                if row.id == TRANSLATE {
                    self.catalog_group.add(&self.translate_row);
                    built.push((row.id.clone(), None));
                } else {
                    let widgets = self.catalog_item(row);
                    self.catalog_group.add(&widgets.row);
                    built.push((row.id.clone(), Some(widgets)));
                }
            }
            self.catalog_widgets.replace(built);
        }
        for ((_, widgets), row) in self.catalog_widgets.borrow().iter().zip(items) {
            let removing = row
                .entry
                .as_ref()
                .is_some_and(|entry| view.removing.contains(&entry.id));
            match widgets {
                Some(widgets) => show_item(widgets, row, view.blocked, removing),
                None => {
                    self.translate_row
                        .set_subtitle(&escape_markup(&row.subtitle));
                    self.translate_row
                        .set_sensitive(matches!(row.standing, Standing::Ready));
                }
            }
        }
    }

    /// The user's own languages, most likely first: the desktop locales,
    /// then the targets of the Translate steps already in a chain.
    pub(crate) fn likely_languages(&self, processing: &Processing) -> Vec<String> {
        let mut likely: Vec<String> = Vec::new();
        let locales = self
            .deps
            .locales
            .iter()
            .map(|l| myna_core::chains::primary_language(l));
        let targets = processing
            .chains
            .iter()
            .flat_map(|c| &c.steps)
            .filter(|s| s.step == TRANSLATE)
            .filter_map(|s| s.params.get("to").cloned());
        for code in locales.chain(targets) {
            if !code.is_empty() && !likely.contains(&code) {
                likely.push(code);
            }
        }
        likely
    }

    pub(crate) fn connect_language_page(self: &Rc<Self>) {
        self.translate_row.connect_activated({
            let page = Rc::downgrade(self);
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.languages.search.set_text("");
                    page.deps.nav.push(&page.languages.page);
                    page.languages.search.grab_focus();
                }
            }
        });
        self.languages.search.connect_search_changed({
            let page = Rc::downgrade(self);
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.filter_languages(true);
                }
            }
        });
        self.languages.search.connect_activate({
            let page = Rc::downgrade(self);
            move |_| {
                if let Some(page) = page.upgrade() {
                    let first = page
                        .languages
                        .rows
                        .borrow()
                        .iter()
                        .find(|(_, w)| w.row.is_visible())
                        .map(|(code, _)| code.clone());
                    if let Some(code) = first {
                        page.choose_language(&code);
                    }
                }
            }
        });
    }

    pub(crate) fn render_languages(
        self: &Rc<Self>,
        from: &Source,
        languages: Vec<Language>,
        removable: &[Entry],
        blocked: bool,
    ) {
        let lp = &self.languages;
        let description = match from {
            Source::One(code) => {
                let frame = gettextrs::gettext(
                    // TRANSLATORS: {language} is the language the text is in where the step goes, such as "English".
                    "From {language}. Choosing a language adds the step; one that is not installed downloads first.",
                );
                frame.replace("{language}", &language_in_text(code))
            }
            Source::Unknown => gettextrs::gettext("Needs a single dictation language"),
        };
        lp.list.set_description(Some(&escape_markup(&description)));
        let same = {
            let rows = lp.rows.borrow();
            rows.len() == languages.len()
                && rows.iter().zip(&languages).all(|((c, _), l)| *c == l.code)
        };
        if !same {
            for (_, widgets) in lp.rows.take() {
                lp.list.remove(&widgets.row);
            }
            lp.list.remove(&lp.empty);
            let mut built = Vec::new();
            for language in &languages {
                let widgets = self.language_row(&language.code);
                lp.list.add(&widgets.row);
                built.push((language.code.clone(), widgets));
            }
            lp.list.add(&lp.empty);
            lp.rows.replace(built);
        }
        for ((_, widgets), language) in lp.rows.borrow().iter().zip(&languages) {
            show_language(widgets, language, blocked);
        }
        self.shown_languages.replace(languages);
        self.filter_languages(false);
        self.render_installed(removable, blocked);
    }

    /// The installed translation models, each with the Remove that takes it
    /// away. A step that uses one stays and says what it needs.
    pub(crate) fn render_installed(self: &Rc<Self>, removable: &[Entry], blocked: bool) {
        let lp = &self.languages;
        let keys: Vec<String> = removable.iter().map(|e| e.id.clone()).collect();
        let same = {
            let rows = lp.installed_rows.borrow();
            rows.len() == keys.len() && rows.iter().zip(&keys).all(|((k, _, _), id)| k == id)
        };
        if !same {
            for (_, row, _) in lp.installed_rows.take() {
                lp.installed.remove(&row);
            }
            let mut built = Vec::new();
            for entry in removable {
                // TRANSLATORS: {from} and {to} are language names, such as "English" and "Français".
                let frame = gettextrs::gettext("{from} to {to}");
                let title = frame
                    .replace("{from}", &language_in_text(&entry.in_lang))
                    .replace("{to}", &language_in_text(&entry.out_lang));
                let row = adw::ActionRow::builder()
                    .title(escape_markup(&title))
                    .subtitle(escape_markup(&size(entry.download_bytes)))
                    .build();
                let remove = gtk::Button::builder()
                    .label(gettextrs::gettext("Remove"))
                    .css_classes(["flat"])
                    .valign(gtk::Align::Center)
                    .build();
                remove.reset_relation(gtk::AccessibleRelation::LabelledBy);
                name(
                    &remove,
                    // TRANSLATORS: {model} names a translation model, such as "English to Français".
                    &gettextrs::gettext("Remove {model}").replace("{model}", &title),
                );
                remove.connect_clicked({
                    let page = Rc::downgrade(self);
                    let entry = entry.clone();
                    move |_| {
                        if let Some(page) = page.upgrade() {
                            page.start_remove(entry.clone());
                        }
                    }
                });
                row.add_suffix(&remove);
                lp.installed.add(&row);
                built.push((entry.id.clone(), row, remove));
            }
            lp.installed_rows.replace(built);
        }
        let removing = !self.removing.borrow().is_empty();
        for (_, _, remove) in lp.installed_rows.borrow().iter() {
            remove.set_sensitive(!blocked && !removing);
        }
        lp.installed.set_visible(!removable.is_empty());
    }

    /// Show the languages the search matches; `announce` says how many.
    pub(crate) fn filter_languages(&self, announce: bool) {
        let lp = &self.languages;
        let query = lp.search.text();
        let shown = self.shown_languages.borrow();
        let mut count = 0u32;
        for ((_, widgets), language) in lp.rows.borrow().iter().zip(shown.iter()) {
            let hit = step_catalog::matches(language, &query);
            widgets.row.set_visible(hit);
            count += u32::from(hit);
        }
        lp.empty.set_visible(count == 0);
        if announce {
            // TRANSLATORS: read by screen readers as the language search narrows; {count} is a number.
            let said = gettextrs::ngettext("{count} language", "{count} languages", count)
                .replace("{count}", &count.to_string());
            lp.search
                .announce(&said, gtk::AccessibleAnnouncementPriority::Medium);
        }
    }

    pub(crate) fn language_row(self: &Rc<Self>, code: &str) -> LanguageWidgets {
        let row = adw::ActionRow::builder().activatable(true).build();
        let state = gtk::Label::builder()
            .css_classes(["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&state);
        row.connect_activated({
            let page = Rc::downgrade(self);
            let code = code.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.choose_language(&code);
                }
            }
        });
        LanguageWidgets { row, state }
    }

    /// Put a Translate step into `code` in the catalog's chain, at its
    /// stage's place, install what it lacks, and go back to the chain.
    pub(crate) fn choose_language(self: &Rc<Self>, code: &str) {
        let Some(language) = self
            .shown_languages
            .borrow()
            .iter()
            .find(|l| l.code == code)
            .cloned()
        else {
            return;
        };
        // A download waits for the dictation or the other operation to end.
        if (self.dictating.get() || self.busy())
            && matches!(language.state, LanguageState::Download { .. })
        {
            return;
        }
        let step = ChainStep::new(TRANSLATE).with("to", code);
        let Some((chain, at)) = self.insert_step(step) else {
            self.leave_catalog();
            return;
        };
        if matches!(language.state, LanguageState::Download { .. }) {
            self.installing_language.replace(Some(code.to_owned()));
            self.install_line(&chain, at);
        }
        self.leave_catalog();
    }

    /// Back from the catalog and its language page to where it was opened.
    pub(crate) fn leave_catalog(&self) {
        let editor = self.editor.borrow().as_ref().map(|e| e.page.clone());
        match editor {
            Some(page) => {
                self.deps.nav.pop_to_page(&page);
            }
            None => {
                let nav = &self.deps.nav;
                while nav.visible_page().as_ref() == Some(&self.languages.page)
                    || nav.visible_page().as_ref() == Some(&self.catalog_page)
                {
                    if !nav.pop() {
                        break;
                    }
                }
            }
        }
    }

    /// Put the step of catalog item `id` in the catalog's chain, at its
    /// stage's place, and go back. With no chain to add to, a new one is
    /// made and used.
    pub(crate) fn add_item(self: &Rc<Self>, id: &str) {
        let Some(row) = self.item(id) else { return };
        if row.on || !matches!(row.standing, Standing::Ready) {
            return;
        }
        let step = step_catalog::step_of(&row, &self.deps.locales);
        if self.insert_step(step).is_none() {
            self.leave_catalog();
            return;
        }
        self.deps.nav.pop();
    }

    pub(crate) fn catalog_item(self: &Rc<Self>, item: &Row) -> CatalogWidgets {
        let row = adw::ExpanderRow::builder()
            .title(escape_markup(&item.title))
            .subtitle_lines(0)
            .build();
        let control = InstallControl::new();
        // Styled and sized as the Install button beside it on other rows.
        let add = gtk::Button::builder()
            .label(gettextrs::gettext("Add"))
            .valign(gtk::Align::Center)
            .width_request(120)
            .css_classes(["outlined"])
            .build();
        // A button is labelled by its label child, over any name set.
        add.reset_relation(gtk::AccessibleRelation::LabelledBy);
        name(
            &add,
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("Add {step}").replace("{step}", &item.title),
        );
        let added = gtk::Label::builder()
            .label(gettextrs::gettext("Added"))
            .css_classes(["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        let suffix = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        suffix.append(&control);
        suffix.append(&add);
        suffix.append(&added);
        row.add_suffix(&suffix);
        catalog_details(&row, item);
        let remove = gtk::Button::builder()
            .label(gettextrs::gettext("Remove"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        remove.reset_relation(gtk::AccessibleRelation::LabelledBy);
        name(
            &remove,
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("Remove {step}").replace("{step}", &item.title),
        );
        let remove_row = adw::ActionRow::builder()
            .title(gettextrs::gettext("Remove this model"))
            .subtitle(gettextrs::gettext(
                "The step stays switched on and shows what it needs, so it can be installed again.",
            ))
            .subtitle_lines(0)
            .visible(false)
            .build();
        remove_row.add_suffix(&remove);
        row.add_row(&remove_row);
        remove.connect_clicked(self.on(&item.id, |page, id, _: &gtk::Button| {
            page.remove_item(id);
        }));
        add.connect_clicked(self.on(&item.id, |page, id, _: &gtk::Button| {
            page.add_item(id);
        }));
        control
            .button()
            .connect_clicked(self.on(&item.id, |page, id, _: &gtk::Button| {
                page.install_item(id);
            }));
        CatalogWidgets {
            row,
            control,
            add,
            added,
            remove_row,
            remove,
        }
    }

    pub(crate) fn remove_item(self: &Rc<Self>, id: &str) {
        if let Some(entry) = self.item(id).and_then(|row| row.removable) {
            self.start_remove(*entry);
        }
    }

    pub(crate) fn item(&self, id: &str) -> Option<Row> {
        self.items.borrow().iter().find(|row| row.id == id).cloned()
    }

    /// An install or a removal is running.
    pub(crate) fn busy(&self) -> bool {
        !self.installing.borrow().is_empty() || !self.removing.borrow().is_empty()
    }

    pub(crate) fn remove_step_model(self: &Rc<Self>, step: &str) {
        let entry = self
            .items
            .borrow()
            .iter()
            .find(|row| row.id == step)
            .and_then(|row| row.removable.clone());
        if let Some(entry) = entry {
            self.start_remove(*entry);
        }
    }

    /// Remove an installed model. The steps stay as they are: a step whose
    /// model is gone says what it needs and the chain skips it.
    pub(crate) fn start_remove(self: &Rc<Self>, entry: Entry) {
        if self.dictating.get() || self.busy() {
            return;
        }
        self.removing.borrow_mut().insert(entry.id.clone());
        self.render();
        let page = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let outcome = run_remove(&page, &entry).await;
            let Some(page) = page.upgrade() else { return };
            page.removing.borrow_mut().remove(&entry.id);
            page.report_outcome(&outcome, &gettextrs::gettext("Removing the model failed"));
            page.refresh().await;
        });
    }

    /// Put `step` at its stage's place in the catalog's chain, making and
    /// using one when none is. `None` when it was opened for a chain that is
    /// gone since: adding to a chain the user never made would be a surprise.
    pub(crate) fn insert_step(self: &Rc<Self>, step: ChainStep) -> Option<(String, usize)> {
        let target = self
            .catalog_chain(&self.deps.store.processing())
            .map(|c| c.id);
        if target.is_none() && self.catalog_for.borrow().is_some() {
            return None;
        }
        let mut placed = None;
        self.edit(|p| {
            let id = match target {
                Some(id) => id,
                None => {
                    let name = chains_view::new_name(&p.chains);
                    let id = p.add(&name, Vec::new());
                    p.activate(Some(&id));
                    id
                }
            };
            if let Some(chain) = p.chains.iter_mut().find(|c| c.id == id) {
                let at = myna_core::chains::insert_position(&chain.steps, &step.step);
                chain.steps.insert(at, step.clone());
                placed = Some((id, at));
            }
        });
        placed
    }

    /// The same install as the inline button, for one catalog entry.
    pub(crate) fn install_item(self: &Rc<Self>, id: &str) {
        let Some(row) = self.item(id) else { return };
        let Standing::Missing(need) = &row.standing else {
            return;
        };
        if self.dictating.get() || self.busy() {
            return;
        }
        let entries = need.entries.clone();
        self.start_install(&row.key, &entries);
    }

    /// Say how an install or a removal went: a success and a dismissed
    /// prompt say nothing.
    pub(crate) fn report_outcome(&self, outcome: &Result<(), Failure>, failed: &str) {
        match outcome {
            Ok(()) | Err(Failure::Dismissed) => {}
            Err(Failure::Dictating) => {
                self.deps
                    .overlay
                    .add_toast(adw::Toast::new(&gettextrs::gettext(
                        "Finish dictating, then try again.",
                    )));
            }
            Err(Failure::Failed(error)) => {
                crate::backend_ui::failure_toast(&self.deps.overlay, failed, error);
            }
        }
    }

    pub(crate) async fn snap_state(&self, snap: &str) -> SnapState {
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
