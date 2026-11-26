//! The Processing tab: the saved chains as a radio
//! list, a chain's editor (its steps in order, each movable, switchable and
//! set up in place), and the catalog the editor adds from. The decisions are
//! `chains_view`, `processing_steps` and `step_catalog`; this binds them to
//! widgets.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;
use myna_core::chains::{ChainStep, Processing};

use crate::adapters::client_settings::GioClientSettings;
use crate::chains_view::{self, Card};
use crate::command::CancellationToken;
use crate::languages::Source;
use crate::markup::escape_markup;
use crate::plugin_catalog::Entry;
use crate::plugin_install::{self};
use crate::ports::{
    BackendRepository, ClientSettingsSubscription, DictationDaemon, SystemConfigurator,
};
use crate::processing_catalog_ui::{language_page, CatalogWidgets, LanguagePage};
use crate::processing_step_ui::{show_line, LineWidgets, Move};
use crate::processing_steps::{Offered, Row, Standing};
use crate::step_catalog::{Language, LanguageState};

/// How long a finished install may wait for the daemon to offer its model
/// before the row offers it again.
/// Below this the chain editor drops its ▴/▾ buttons (the row menu keeps
/// Move up/down): at the window's 360 px minimum they left a step's title
/// a letter per line.
const NARROW_EDITOR: &str = "max-width: 560sp";

const PREPARING_FOR: Duration = Duration::from_secs(30);

const PAGE_KEY: &str = "myna-steps-page";

pub struct Deps {
    pub store: Rc<GioClientSettings>,
    pub daemon: Rc<dyn DictationDaemon>,
    pub configurator: Rc<dyn SystemConfigurator>,
    pub repository: Rc<dyn BackendRepository>,
    pub catalog: Box<dyn Fn() -> Vec<Entry>>,
    pub locales: Vec<String>,
    pub overlay: adw::ToastOverlay,
    /// Where the editor and the catalog are pushed.
    pub nav: adw::NavigationView,
    /// How often an install's change is read for its progress.
    pub follow_interval: Duration,
}

/// The groups of the Processing tab the page fills.
pub struct Slots {
    /// The chains: none, each saved chain, New chain.
    pub chains: adw::PreferencesGroup,
    /// Get more steps goes first in it.
    pub more: adw::PreferencesGroup,
}

/// One saved chain on the page.
struct ChainRow {
    row: adw::ActionRow,
    /// On for the chain in use; turning one on turns the others off.
    switch: gtk::Switch,
    delete: gtk::Button,
}

/// The open editor of one chain.
pub(crate) struct Editor {
    pub(crate) id: String,
    pub(crate) page: adw::NavigationPage,
    pub(crate) name: adw::EntryRow,
    pub(crate) steps: adw::PreferencesGroup,
    pub(crate) add_row: adw::ActionRow,
    pub(crate) use_button: gtk::Button,
    pub(crate) delete: gtk::Button,
    pub(crate) lines: RefCell<Vec<LineWidgets>>,
    pub(crate) shown: RefCell<Vec<Row>>,
    /// Below the editor's breakpoint: the ▴/▾ buttons hide (the row menu's
    /// Move up/down remain) and a step's parameter picker moves to the row
    /// under it, so the title keeps its room.
    pub(crate) narrow: Rc<Cell<bool>>,
}

/// A chain's editor before anything is wired: its name, its steps, and the
/// two buttons under them, in the page its breakpoint watches.
fn editor_page(id: &str) -> (adw::NavigationPage, Rc<Editor>, adw::Breakpoint) {
    let name = adw::EntryRow::builder()
        .title(gettextrs::gettext("Name"))
        .show_apply_button(true)
        .build();
    let name_group = adw::PreferencesGroup::new();
    name_group.add(&name);
    let steps = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Steps"))
        .description(gettextrs::gettext(
            "They run top to bottom. Changes are saved as you make them.",
        ))
        .build();
    let add_row = adw::ActionRow::builder()
        .title(gettextrs::gettext("Add step…"))
        .activatable(true)
        .build();
    add_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
    let use_button = gtk::Button::builder()
        .label(gettextrs::gettext("Use this chain"))
        .css_classes(["suggested-action", "pill"])
        .halign(gtk::Align::Center)
        .build();
    let delete = gtk::Button::builder()
        .label(gettextrs::gettext("Delete chain"))
        .css_classes(["destructive-action", "pill"])
        .halign(gtk::Align::Center)
        .build();
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    actions.append(&use_button);
    actions.append(&delete);
    let actions_group = adw::PreferencesGroup::new();
    actions_group.add(&actions);
    let preferences = adw::PreferencesPage::new();
    for group in [&name_group, &steps, &actions_group] {
        preferences.add(group);
    }
    let bin = adw::BreakpointBin::builder()
        .width_request(300)
        .height_request(200)
        .child(&preferences)
        .build();
    let breakpoint = adw::Breakpoint::new(
        adw::BreakpointCondition::parse(NARROW_EDITOR).expect("a valid breakpoint condition"),
    );
    bin.add_breakpoint(breakpoint.clone());
    let page = crate::subpage::subpage(&gettextrs::gettext("Chain"), &bin);
    let editor = Rc::new(Editor {
        id: id.to_owned(),
        page: page.clone(),
        name,
        steps,
        add_row,
        use_button,
        delete,
        lines: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        narrow: Rc::new(Cell::new(false)),
    });
    (page, editor, breakpoint)
}

pub struct StepsPage {
    pub(crate) deps: Deps,
    pub(crate) chains_group: glib::WeakRef<adw::PreferencesGroup>,
    /// Never shown: the chain radios' group, active while no chain is, so
    /// even a lone chain's radio is a radio and not a checkbox.
    /// In place of the chains while there are none.
    pub(crate) empty_row: adw::ActionRow,
    pub(crate) new_row: adw::ActionRow,
    chain_rows: RefCell<Vec<(String, ChainRow)>>,
    pub(crate) shown_cards: RefCell<Vec<Card>>,
    pub(crate) editor: RefCell<Option<Rc<Editor>>>,
    pub(crate) catalog_page: adw::NavigationPage,
    pub(crate) catalog_group: adw::PreferencesGroup,
    /// The catalog's rows by job, in order; `None` is the Translate row.
    pub(crate) catalog_widgets: RefCell<Vec<(String, Option<CatalogWidgets>)>>,
    /// The catalog's Translate row: it opens the language page.
    pub(crate) translate_row: adw::ActionRow,
    pub(crate) languages: LanguagePage,
    /// The languages the language page lists now.
    pub(crate) shown_languages: RefCell<Vec<Language>>,
    /// The language whose models the last choice is installing.
    pub(crate) installing_language: RefCell<Option<String>>,
    /// The chain the catalog adds to: the one being edited, else the
    /// active one.
    pub(crate) catalog_for: RefCell<Option<String>>,
    pub(crate) items: RefCell<Vec<Row>>,
    /// A dictation is running: installs wait.
    pub(crate) dictating: Cell<bool>,
    pub(crate) catalog: RefCell<Vec<Entry>>,
    pub(crate) offered: RefCell<Option<Offered>>,
    pub(crate) installing: RefCell<BTreeMap<String, Option<u8>>>,
    pub(crate) preparing: RefCell<BTreeMap<String, Instant>>,
    /// Catalog entry ids whose models are being removed.
    pub(crate) removing: RefCell<BTreeSet<String>>,
    /// Set while the page writes a value into a widget, so the widget's
    /// signal is not taken for the user.
    pub(crate) updating: Cell<bool>,
    pub(crate) cancellation: CancellationToken,
    pub(crate) watch: RefCell<Option<Box<dyn ClientSettingsSubscription>>>,
}

impl Drop for StepsPage {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Name a widget for assistive technology, which reads this instead of the
/// pixels.
pub(crate) fn name<W: IsA<gtk::Accessible>>(widget: &W, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Label(text)]);
}

/// Describe one the same way, where the name alone says too little.
pub(crate) fn describe<W: IsA<gtk::Accessible>>(widget: &W, text: &str) {
    widget.update_property(&[gtk::accessible::Property::Description(text)]);
}

fn link_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(escape_markup(title))
        .subtitle(escape_markup(subtitle))
        .subtitle_lines(0)
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

impl StepsPage {
    /// A signal handler that runs `f` on the page with the chain the widget
    /// was built for, if the page is still alive: a widget outlives it. It
    /// runs nothing while the page writes the control itself.
    pub(crate) fn on<T>(
        self: &Rc<Self>,
        id: &str,
        f: impl Fn(&Rc<Self>, &str, &T) + 'static,
    ) -> impl Fn(&T) + 'static {
        let page = Rc::downgrade(self);
        let id = id.to_owned();
        move |widget: &T| {
            let Some(page) = page.upgrade() else { return };
            if !page.updating.get() {
                f(&page, &id, widget);
            }
        }
    }

    /// A signal handler with no chain, for the page's own rows.
    pub(crate) fn act<T>(
        self: &Rc<Self>,
        f: impl Fn(&Rc<Self>) + 'static,
    ) -> impl Fn(&T) + 'static {
        self.on::<T>("", move |page, _, _| f(page))
    }

    /// Fill the slots and keep them current for as long as the chains group
    /// lives: the group owns the page, the page only watches the groups.
    pub fn attach(deps: Deps, slots: &Slots) -> Rc<Self> {
        let empty_row = adw::ActionRow::builder()
            .title(gettextrs::gettext("No chains yet"))
            .subtitle(gettextrs::gettext(
                "Myna types exactly what you say. Make a chain to change that.",
            ))
            .subtitle_lines(0)
            .build();
        let new_row = adw::ActionRow::builder()
            .title(gettextrs::gettext("New chain…"))
            .activatable(true)
            .build();
        new_row.add_prefix(&gtk::Image::from_icon_name("list-add-symbolic"));
        let more_row = link_row(
            &gettextrs::gettext("Get more steps"),
            &gettextrs::gettext("Translation languages, clean-up, scripts"),
        );
        slots.more.add(&more_row);
        let catalog_group = adw::PreferencesGroup::builder()
            .description(gettextrs::gettext(
                "Steps your catalogs offer. Add one to put it in the chain; one that is not installed downloads first.",
            ))
            .build();
        let preferences = adw::PreferencesPage::new();
        preferences.add(&catalog_group);
        let catalog_page = crate::subpage::subpage(&gettextrs::gettext("Add step"), &preferences);
        let translate_row = link_row(&gettextrs::gettext("Translate"), "");
        let languages = language_page();
        let catalog = (deps.catalog)();
        let page = Rc::new(Self {
            deps,
            chains_group: slots.chains.downgrade(),
            empty_row,
            new_row,
            chain_rows: RefCell::new(Vec::new()),
            shown_cards: RefCell::new(Vec::new()),
            editor: RefCell::new(None),
            catalog_page,
            catalog_group,
            catalog_widgets: RefCell::new(Vec::new()),
            translate_row,
            languages,
            shown_languages: RefCell::new(Vec::new()),
            installing_language: RefCell::new(None),
            catalog_for: RefCell::new(None),
            items: RefCell::new(Vec::new()),
            dictating: Cell::new(false),
            catalog: RefCell::new(catalog),
            offered: RefCell::new(None),
            installing: RefCell::new(BTreeMap::new()),
            preparing: RefCell::new(BTreeMap::new()),
            removing: RefCell::new(BTreeSet::new()),
            updating: Cell::new(false),
            cancellation: CancellationToken::new(),
            watch: RefCell::new(None),
        });
        slots.chains.add(&page.empty_row);
        slots.chains.add(&page.new_row);
        let weak = Rc::downgrade(&page);
        page.watch
            .replace(Some(page.deps.store.watch_steps(move || {
                if let Some(page) = weak.upgrade() {
                    page.render();
                }
            })));
        page.new_row
            .connect_activated(page.act(|page| page.new_chain()));
        more_row.connect_activated(page.act(|page| page.open_catalog(None)));
        page.connect_language_page();
        page.render();
        // SAFETY: the key is private to this module and always holds a page.
        unsafe { slots.chains.set_data(PAGE_KEY, page.clone()) };
        page.follow();
        page
    }

    /// Follow the daemon's own property changes, and read everything once: the
    /// catalogs on opening the page, the daemon on every change it publishes.
    pub(crate) fn follow(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let on_change: Rc<dyn Fn()> = Rc::new(move || {
            let Some(page) = weak.upgrade() else { return };
            glib::spawn_future_local(async move {
                page.read_daemon().await;
                page.render();
            });
        });
        let daemon = self.deps.daemon.clone();
        glib::spawn_future_local(async move {
            daemon.watch(on_change).await;
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(page) = weak.upgrade() else { return };
            page.refresh().await;
        });
    }

    /// Read the daemon and the catalogs, and redraw: on opening the page and
    /// after an install, never on a timer.
    pub async fn refresh(self: &Rc<Self>) {
        self.read_daemon().await;
        self.catalog.replace((self.deps.catalog)());
        self.render();
    }

    /// What the daemon offers and whether it dictates.
    async fn read_daemon(self: &Rc<Self>) {
        let offered = match self.deps.daemon.installed_steps().await {
            Ok(Some(json)) => Offered::parse(&json),
            _ => None,
        };
        self.offered.replace(offered);
        let busy = match self.deps.daemon.state().await {
            Ok(Some(state)) => plugin_install::dictating(&state),
            _ => false,
        };
        self.dictating.set(busy);
    }

    /// Change the stored chains in one write; a failure is a toast.
    pub(crate) fn edit(self: &Rc<Self>, change: impl FnOnce(&mut Processing)) {
        let mut processing = self.deps.store.processing();
        change(&mut processing);
        if let Err(error) = self.deps.store.set_processing(&processing) {
            glib::g_warning!(crate::LOG_DOMAIN, "chains not stored: {error}");
            self.deps
                .overlay
                .add_toast(adw::Toast::new(&gettextrs::gettext(
                    "The change could not be saved.",
                )));
        }
        self.render();
    }

    /// Change the steps of chain `id`.
    pub(crate) fn edit_steps(self: &Rc<Self>, id: &str, change: impl FnOnce(&mut Vec<ChainStep>)) {
        self.edit(|p| {
            if let Some(chain) = p.chains.iter_mut().find(|c| c.id == id) {
                change(&mut chain.steps);
            }
        });
    }

    /// Whether `change` to chain `id` would leave a translation no model
    /// can do: the editor never makes one.
    pub(crate) fn would_strand(
        &self,
        id: &str,
        change: impl FnOnce(&[ChainStep]) -> Vec<ChainStep>,
    ) -> bool {
        let Some(chain) = self.deps.store.processing().chain(id).cloned() else {
            return false;
        };
        let after = change(&chain.steps);
        self.view(|ctx| chains_view::strands(&chain.steps, &after, ctx))
    }

    pub(crate) fn dictation_language(&self) -> Option<String> {
        let language = self.deps.store.language();
        match Source::of_setting(&language) {
            Source::One(code) => Some(code),
            Source::Unknown => None,
        }
    }

    pub(crate) fn preparing_now(&self) -> BTreeSet<String> {
        let now = Instant::now();
        self.preparing
            .borrow()
            .iter()
            .filter(|(_, since)| now.duration_since(**since) < PREPARING_FOR)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The view of `chains` given what is installed now.
    pub(crate) fn view<T>(&self, f: impl FnOnce(&chains_view::Context) -> T) -> T {
        let offered = self.offered.borrow().clone();
        let catalog = self.catalog.borrow();
        let installing = self.installing.borrow();
        let preparing = self.preparing_now();
        let language = self.dictation_language();
        f(&chains_view::Context {
            language: language.as_deref(),
            catalog: &catalog,
            daemon: offered.as_ref(),
            installing: &installing,
            preparing: &preparing,
            locales: &self.deps.locales,
        })
    }

    pub(crate) fn render(self: &Rc<Self>) {
        let processing = self.deps.store.processing();
        self.render_chains(&processing);
        self.render_editor(&processing);
        self.render_catalog(&processing);
    }

    pub(crate) fn render_chains(self: &Rc<Self>, processing: &Processing) {
        let Some(group) = self.chains_group.upgrade() else {
            return;
        };
        let active = processing.active().map(|c| c.id.clone());
        let cards = self.view(|ctx| chains_view::cards(&processing.chains, active.as_deref(), ctx));
        let same_ids = {
            let rows = self.chain_rows.borrow();
            rows.len() == cards.len() && rows.iter().zip(&cards).all(|((id, _), c)| *id == c.id)
        };
        if !same_ids {
            for (_, chain) in self.chain_rows.take() {
                group.remove(&chain.row);
            }
            group.remove(&self.new_row);
            let mut built = Vec::new();
            for card in &cards {
                let chain = self.chain_row(&card.id);
                group.add(&chain.row);
                built.push((card.id.clone(), chain));
            }
            group.add(&self.new_row);
            self.chain_rows.replace(built);
        }
        self.updating.set(true);
        for ((_, chain), card) in self.chain_rows.borrow().iter().zip(&cards) {
            chain.row.set_title(&escape_markup(&card.name));
            let subtitle = match &card.needs {
                Some(needs) => format!("{}\n{needs}", card.steps),
                None => card.steps.clone(),
            };
            chain.row.set_subtitle(&escape_markup(&subtitle));
            chain.switch.set_active(card.active);
            // TRANSLATORS: the accessible name of a chain's on/off switch; {name} is the chain's name.
            let use_it = gettextrs::gettext("Use {name}").replace("{name}", &card.name);
            name(&chain.switch, &use_it);
            // TRANSLATORS: the accessible name of a chain's delete button; {name} is the chain's name.
            let delete = gettextrs::gettext("Delete {name}").replace("{name}", &card.name);
            name(&chain.delete, &delete);
            chain.delete.set_tooltip_text(Some(&delete));
        }
        self.empty_row.set_visible(cards.is_empty());
        self.updating.set(false);
        self.shown_cards.replace(cards);
    }

    /// A chain on the page: opens its editor, marks the one in use, and
    /// deletes with an Undo toast.
    fn chain_row(self: &Rc<Self>, id: &str) -> ChainRow {
        let row = adw::ActionRow::builder()
            .subtitle_lines(0)
            .activatable(true)
            .build();
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&switch);
        row.add_suffix(&delete);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        switch.connect_active_notify(self.on(id, |page, id, switch: &gtk::Switch| {
            let on = switch.is_active();
            page.edit(|p| {
                if on {
                    p.activate(Some(id));
                } else if p.active.as_deref() == Some(id) {
                    p.activate(None);
                }
            });
        }));
        row.connect_activated(self.on(id, |page, id, _: &adw::ActionRow| page.open_editor(id)));
        delete.connect_clicked(self.on(id, |page, id, _: &gtk::Button| page.delete_chain(id)));
        ChainRow {
            row,
            switch,
            delete,
        }
    }

    /// A copy of the active chain under a new name, opened for editing.
    pub(crate) fn new_chain(self: &Rc<Self>) {
        let mut id = String::new();
        self.edit(|p| {
            let steps = p.active().map(|c| c.steps.clone()).unwrap_or_default();
            let name = chains_view::new_name(&p.chains);
            id = p.add(&name, steps);
        });
        self.open_editor(&id);
    }

    pub(crate) fn open_editor(self: &Rc<Self>, id: &str) {
        let (page, editor, breakpoint) = editor_page(id);
        self.wire_editor(id, &editor, &breakpoint);
        if let Some(chain) = self.deps.store.processing().chain(id) {
            editor.name.set_text(&chain.name);
            page.set_title(&chain.name);
        }
        self.editor.replace(Some(editor));
        self.deps.nav.push(&page);
        self.render();
    }

    /// What the editor's own rows do: its name, Add step, its two buttons,
    /// and the breakpoint that makes the steps build their pickers again.
    pub(crate) fn wire_editor(
        self: &Rc<Self>,
        id: &str,
        editor: &Rc<Editor>,
        breakpoint: &adw::Breakpoint,
    ) {
        for (signal, value) in [("apply", true), ("unapply", false)] {
            let page = Rc::downgrade(self);
            let narrow = editor.narrow.clone();
            breakpoint.connect_local(signal, false, move |_| {
                narrow.set(value);
                if let Some(page) = page.upgrade() {
                    // The pickers change rows: build the steps again.
                    if let Some(editor) = page.editor.borrow().clone() {
                        let count = editor.lines.borrow().len();
                        page.rebuild_lines(&editor, count);
                    }
                    page.render();
                }
                None
            });
        }
        editor
            .name
            .connect_apply(self.on(id, |page, id, entry: &adw::EntryRow| {
                let text = entry.text().trim().to_owned();
                if !text.is_empty() {
                    page.edit(|p| {
                        p.rename(id, &text);
                    });
                }
            }));
        editor
            .add_row
            .connect_activated(self.on(id, |page, id, _: &adw::ActionRow| {
                page.open_catalog(Some(id));
            }));
        editor
            .use_button
            .connect_clicked(self.on(id, |page, id, _: &gtk::Button| {
                page.edit(|p| {
                    let using = p.active.as_deref() == Some(id);
                    p.activate((!using).then_some(id));
                });
            }));
        editor
            .delete
            .connect_clicked(self.on(id, |page, id, _: &gtk::Button| page.delete_chain(id)));
        let page = editor.page.clone();
        page.connect_hidden(self.on(id, |page, id, _: &adw::NavigationPage| {
            let open = page.editor.borrow().as_ref().map(|e| e.id.clone());
            if open.as_deref() == Some(id)
                && page.deps.nav.visible_page().as_ref() != Some(&page.catalog_page)
            {
                page.editor.replace(None);
            }
        }));
    }

    /// Delete chain `id` and go back; a toast offers it back.
    pub(crate) fn delete_chain(self: &Rc<Self>, id: &str) {
        let before = self.deps.store.processing();
        let Some(at) = before.chains.iter().position(|c| c.id == id) else {
            return;
        };
        let chain = before.chains[at].clone();
        let was_active = before.active.as_deref() == Some(id);
        self.edit(|p| {
            p.delete(id);
        });
        self.editor.replace(None);
        self.deps.nav.pop();
        let toast = adw::Toast::builder()
            // TRANSLATORS: {name} is the name of a chain of steps that was just deleted.
            .title(escape_markup(
                &gettextrs::gettext("{name} deleted").replace("{name}", &chain.name),
            ))
            .button_label(gettextrs::gettext("Undo"))
            .build();
        toast.connect_button_clicked(self.act(move |page| {
            let chain = chain.clone();
            page.edit(|p| {
                let at = at.min(p.chains.len());
                if p.chain(&chain.id).is_none() {
                    p.chains.insert(at, chain.clone());
                    if was_active {
                        p.activate(Some(&chain.id));
                    }
                }
            });
        }));
        self.deps.overlay.add_toast(toast);
    }

    pub(crate) fn render_editor(self: &Rc<Self>, processing: &Processing) {
        let Some(editor) = self.editor.borrow().clone() else {
            return;
        };
        let Some(chain) = processing.chain(&editor.id) else {
            self.editor.replace(None);
            return;
        };
        if !editor.name.has_focus() && editor.name.text() != chain.name {
            editor.name.set_text(&chain.name);
        }
        editor.page.set_title(&chain.name);
        let active = processing.active.as_deref() == Some(chain.id.as_str());
        // Stopping is the way back to typing what was heard, now that the
        // page has no None row: no accent on it.
        if active {
            editor.use_button.remove_css_class("suggested-action");
            editor
                .use_button
                .set_label(&gettextrs::gettext("Stop using"));
        } else {
            editor.use_button.add_css_class("suggested-action");
            editor
                .use_button
                .set_label(&gettextrs::gettext("Use this chain"));
        }
        let lines = self.view(|ctx| chains_view::lines(chain, ctx));
        // A new editor has no rows at all, not even Add step, whatever the
        // chain holds.
        let rebuild = editor.add_row.parent().is_none() || {
            let built = editor.lines.borrow();
            built.len() != lines.len()
                || editor
                    .shown
                    .borrow()
                    .iter()
                    .zip(&lines)
                    .any(|(a, b)| a.id != b.id)
        };
        if rebuild {
            self.rebuild_lines(&editor, lines.len());
        }
        self.updating.set(true);
        let total = lines.len();
        let blocked = self.dictating.get() || self.busy();
        let items = self.items.borrow().clone();
        for (widgets, line) in editor.lines.borrow().iter().zip(&lines) {
            show_line(widgets, line, total, blocked, editor.narrow.get(), &items);
        }
        self.updating.set(false);
        editor.shown.replace(lines);
    }

    pub(crate) fn rebuild_lines(self: &Rc<Self>, editor: &Rc<Editor>, count: usize) {
        for widgets in editor.lines.take() {
            editor.steps.remove(&widgets.row);
            editor.steps.remove(&widgets.detail);
        }
        if editor.add_row.parent().is_some() {
            editor.steps.remove(&editor.add_row);
        }
        let mut built = Vec::new();
        for index in 0..count {
            let widgets = self.line_widgets(&editor.id, index, editor.narrow.get());
            editor.steps.add(&widgets.row);
            editor.steps.add(&widgets.detail);
            built.push(widgets);
        }
        editor.steps.add(&editor.add_row);
        editor.lines.replace(built);
    }

    /// Move step `from` of chain `id` one of `mv`, say so, and keep the focus
    /// on the step that moved.
    pub(crate) fn move_by(self: &Rc<Self>, id: &str, from: usize, mv: Move, focus: Focus) {
        let len = self
            .deps
            .store
            .processing()
            .chain(id)
            .map_or(0, |c| c.steps.len());
        if let Some(to) = mv.target(from, len) {
            self.move_step(id, from, to, focus);
        }
    }

    /// Move step `from` of chain `id` to `to`, say so, and keep the focus on
    /// the step that moved.
    pub(crate) fn move_step(self: &Rc<Self>, id: &str, from: usize, to: usize, focus: Focus) {
        let len = self
            .deps
            .store
            .processing()
            .chain(id)
            .map_or(0, |c| c.steps.len());
        if from == to
            || from >= len
            || to >= len
            || self.would_strand(id, |steps| chains_view::moved(steps, from, to))
        {
            return;
        }
        self.edit_steps(id, |steps| *steps = chains_view::moved(steps, from, to));
        let Some(editor) = self.editor.borrow().clone() else {
            return;
        };
        let lines = editor.lines.borrow();
        let shown = editor.shown.borrow();
        let (Some(widgets), Some(line)) = (lines.get(to), shown.get(to)) else {
            return;
        };
        // TRANSLATORS: said after a step moved; {step} is its name, {n} and {total} its new place.
        let said = gettextrs::gettext("{step} moved to step {n} of {total}")
            .replace("{step}", &line.title)
            .replace("{n}", &(to + 1).to_string())
            .replace("{total}", &len.to_string());
        widgets
            .row
            .announce(&said, gtk::AccessibleAnnouncementPriority::Medium);
        let target: gtk::Widget = match focus {
            Focus::Up if widgets.up.is_sensitive() => widgets.up.clone().upcast(),
            Focus::Down if widgets.down.is_sensitive() => widgets.down.clone().upcast(),
            _ => widgets.row.clone().upcast(),
        };
        target.grab_focus();
    }

    /// The picker of step `index` chose its entry `choice`: a translation's
    /// target (installing it when missing) or a script.
    pub(crate) fn choose(self: &Rc<Self>, id: &str, index: usize, choice: u32) {
        let line = self
            .editor
            .borrow()
            .as_ref()
            .and_then(|e| e.shown.borrow().get(index).cloned());
        let Some(line) = line else { return };
        if let Some(translate) = &line.translate {
            let Some(target) = translate.targets.get(choice as usize).cloned() else {
                return;
            };
            if self.would_strand(id, |steps| {
                let mut steps = steps.to_vec();
                if let Some(step) = steps.get_mut(index) {
                    step.params.insert("to".into(), target.code.clone());
                }
                steps
            }) {
                return;
            }
            self.edit_steps(id, |steps| {
                if let Some(step) = steps.get_mut(index) {
                    step.params.insert("to".into(), target.code.clone());
                }
            });
            if matches!(target.state, LanguageState::Download { .. }) {
                self.install_line(id, index);
            }
        } else if let Some(script) = &line.script {
            let Some((nick, _)) = script.modes.get(choice as usize).cloned() else {
                return;
            };
            self.edit_steps(id, |steps| {
                if let Some(step) = steps.get_mut(index) {
                    step.params.insert("mode".into(), nick);
                }
            });
        }
    }

    /// Install what step `index` of chain `id` lacks.
    pub(crate) fn install_line(self: &Rc<Self>, id: &str, index: usize) {
        let processing = self.deps.store.processing();
        let Some(chain) = processing.chain(id) else {
            return;
        };
        let lines = self.view(|ctx| chains_view::lines(chain, ctx));
        let Some(line) = lines.get(index) else { return };
        if let Standing::Missing(need) = &line.standing {
            self.start_install(&line.id, &need.entries);
        }
    }
}

/// Where the focus goes after a step moved.
#[derive(Clone, Copy)]
pub(crate) enum Focus {
    Up,
    Down,
    Row,
}
