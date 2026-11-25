//! The Processing tab: the fit-to-field switch, the saved chains as a radio
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
use myna_core::chains::{Chain, ChainStep, Processing};

use crate::adapters::client_settings::GioClientSettings;
use crate::chains_view::{self, Card, Line};
use crate::command::CancellationToken;
use crate::markup::escape_markup;
use crate::plugin_catalog::{self, Entry};
use crate::plugin_install::{self, Failure, SnapState};
use crate::ports::{
    BackendRepository, ClientSettingsSubscription, DictationDaemon, SystemConfigurator,
};
use crate::processing_steps::{
    example, language_name, rows, size, Inputs, Offered, Row, Source, Standing, SCRIPT, TRANSLATE,
};
use crate::step_catalog::{self, Item, Job, Language, LanguageState, Offer};
use crate::ui::InstallControl;

/// How long a finished install may wait for the daemon to offer its model
/// before the row offers it again.
/// Below this the chain editor drops its ▴/▾ buttons (the row menu keeps
/// Move up/down): at the window's 360 px minimum they left a step's title
/// a letter per line.
const NARROW_EDITOR: &str = "max-width: 560sp";

const PREPARING_FOR: Duration = Duration::from_secs(30);

const PAGE_KEY: &str = "myna-steps-page";

/// How often the daemon is asked what it offers.
const REFRESH_EVERY: Duration = Duration::from_secs(5);

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
    /// How often the daemon is asked what it offers.
    pub refresh_every: Duration,
}

impl Deps {
    pub fn refresh_default() -> Duration {
        REFRESH_EVERY
    }
}

/// The groups of the Processing tab the page fills.
pub struct Slots {
    /// The fit-to-field switch, on top.
    pub fit: adw::PreferencesGroup,
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

/// One step in the editor.
struct LineWidgets {
    row: adw::ActionRow,
    /// "Skipped", shown while the text at the step's place does not suit it.
    skipped: gtk::Label,
    /// Under the step, only when it has something to set or do: its
    /// parameter picker, its install and its fix. On the step's own row
    /// they left the title no room.
    detail: adw::ActionRow,
    number: gtk::Label,
    badge: gtk::Label,
    picker: gtk::DropDown,
    /// What the picker lists now, to refill it only when it changes.
    listed: RefCell<Vec<String>>,
    control: InstallControl,
    fix: gtk::Button,
    switch: gtk::Switch,
    up: gtk::Button,
    down: gtk::Button,
    move_up: gtk::Button,
    move_down: gtk::Button,
    to_top: gtk::Button,
    to_bottom: gtk::Button,
    menu: gtk::MenuButton,
    remove_model: gtk::Button,
}

/// The open editor of one chain.
struct Editor {
    id: String,
    page: adw::NavigationPage,
    name: adw::EntryRow,
    steps: adw::PreferencesGroup,
    add_row: adw::ActionRow,
    example: adw::ActionRow,
    use_button: gtk::Button,
    lines: RefCell<Vec<LineWidgets>>,
    shown: RefCell<Vec<Line>>,
    /// Below the editor's breakpoint: the ▴/▾ buttons hide (the row menu's
    /// Move up/down remain) and a step's parameter picker moves to the row
    /// under it, so the title keeps its room.
    narrow: Rc<Cell<bool>>,
}

/// The page Translate opens: a search over the languages the text can be
/// translated into, and the installed translation models to remove.
struct LanguagePage {
    page: adw::NavigationPage,
    search: gtk::SearchEntry,
    /// Says which language the text is in where the step would go.
    list: adw::PreferencesGroup,
    empty: adw::ActionRow,
    rows: RefCell<Vec<(String, LanguageWidgets)>>,
    installed: adw::PreferencesGroup,
    installed_rows: RefCell<Vec<(String, adw::ActionRow, gtk::Button)>>,
}

struct LanguageWidgets {
    row: adw::ActionRow,
    state: gtk::Label,
}

/// One item of the catalog page.
struct CatalogWidgets {
    row: adw::ExpanderRow,
    control: InstallControl,
    add: gtk::Button,
    added: gtk::Label,
    remove_row: adw::ActionRow,
    remove: gtk::Button,
}

pub struct StepsPage {
    deps: Deps,
    fit_row: adw::SwitchRow,
    chains_group: glib::WeakRef<adw::PreferencesGroup>,
    /// Never shown: the chain radios' group, active while no chain is, so
    /// even a lone chain's radio is a radio and not a checkbox.
    /// In place of the chains while there are none.
    empty_row: adw::ActionRow,
    new_row: adw::ActionRow,
    chain_rows: RefCell<Vec<(String, ChainRow)>>,
    shown_cards: RefCell<Vec<Card>>,
    editor: RefCell<Option<Rc<Editor>>>,
    catalog_page: adw::NavigationPage,
    catalog_group: adw::PreferencesGroup,
    /// The catalog's rows by job, in order; `None` is the Translate row.
    catalog_widgets: RefCell<Vec<(String, Option<CatalogWidgets>)>>,
    /// The catalog's Translate row: it opens the language page.
    translate_row: adw::ActionRow,
    languages: LanguagePage,
    /// The languages the language page lists now.
    shown_languages: RefCell<Vec<Language>>,
    /// The language whose models the last choice is installing.
    installing_language: RefCell<Option<String>>,
    /// The chain the catalog adds to: the one being edited, else the
    /// active one.
    catalog_for: RefCell<Option<String>>,
    items: RefCell<Vec<Item>>,
    /// A dictation is running: installs wait.
    dictating: Cell<bool>,
    catalog: RefCell<Vec<Entry>>,
    offered: RefCell<Option<Offered>>,
    installing: RefCell<BTreeMap<String, Option<u8>>>,
    preparing: RefCell<BTreeMap<String, Instant>>,
    /// Catalog entry ids whose models are being removed.
    removing: RefCell<BTreeSet<String>>,
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
    /// Fill the slots and keep them current for as long as the chains group
    /// lives: the group owns the page, the page only watches the groups.
    pub fn attach(deps: Deps, slots: &Slots) -> Rc<Self> {
        let fit_row = adw::SwitchRow::builder()
            .title(gettextrs::gettext("Fitted to the field"))
            .subtitle(gettextrs::gettext(
                "No capital or full stop in an address, and no capital where you carry on a sentence.",
            ))
            .subtitle_lines(0)
            .build();
        slots.fit.add(&fit_row);
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
            fit_row,
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
        page.fit_row.connect_active_notify({
            let page = Rc::downgrade(&page);
            move |row| {
                if let Some(page) = page.upgrade() {
                    if !page.updating.get() {
                        let on = row.is_active();
                        page.edit(|p| p.fit_to_field = on);
                    }
                }
            }
        });
        page.new_row.connect_activated({
            let page = Rc::downgrade(&page);
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.new_chain();
                }
            }
        });
        more_row.connect_activated({
            let page = Rc::downgrade(&page);
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.open_catalog(None);
                }
            }
        });
        page.connect_language_page();
        page.render();
        // SAFETY: the key is private to this module and always holds a page.
        unsafe { slots.chains.set_data(PAGE_KEY, page.clone()) };
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
        let busy = match self.deps.daemon.state().await {
            Ok(Some(state)) => plugin_install::dictating(&state),
            _ => false,
        };
        self.dictating.set(busy);
        self.catalog.replace((self.deps.catalog)());
        self.render();
    }

    /// Change the stored chains in one write; a failure is a toast.
    fn edit(self: &Rc<Self>, change: impl FnOnce(&mut Processing)) {
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
    fn edit_steps(self: &Rc<Self>, id: &str, change: impl FnOnce(&mut Vec<ChainStep>)) {
        self.edit(|p| {
            if let Some(chain) = p.chains.iter_mut().find(|c| c.id == id) {
                change(&mut chain.steps);
            }
        });
    }

    /// Whether `change` to chain `id` would leave a translation no model
    /// can do: the editor never makes one.
    fn would_strand(&self, id: &str, change: impl FnOnce(&[ChainStep]) -> Vec<ChainStep>) -> bool {
        let Some(chain) = self.deps.store.processing().chain(id).cloned() else {
            return false;
        };
        let after = change(&chain.steps);
        self.view(|ctx| chains_view::strands(&chain.steps, &after, ctx))
    }

    fn dictation_language(&self) -> Option<String> {
        let language = self.deps.store.language();
        match Source::of_setting(&language) {
            Source::One(code) => Some(code),
            Source::Unknown => None,
        }
    }

    fn preparing_now(&self) -> BTreeSet<String> {
        let now = Instant::now();
        self.preparing
            .borrow()
            .iter()
            .filter(|(_, since)| now.duration_since(**since) < PREPARING_FOR)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The view of `chains` given what is installed now.
    fn view<T>(&self, f: impl FnOnce(&chains_view::Context) -> T) -> T {
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

    fn render(self: &Rc<Self>) {
        let processing = self.deps.store.processing();
        self.updating.set(true);
        self.fit_row.set_active(processing.fit_to_field);
        self.updating.set(false);
        self.render_chains(&processing);
        self.render_editor(&processing);
        self.render_catalog(&processing);
    }

    fn render_chains(self: &Rc<Self>, processing: &Processing) {
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
            chain
                .switch
                .update_property(&[gtk::accessible::Property::Label(&use_it)]);
            // TRANSLATORS: the accessible name of a chain's delete button; {name} is the chain's name.
            let delete = gettextrs::gettext("Delete {name}").replace("{name}", &card.name);
            chain
                .delete
                .update_property(&[gtk::accessible::Property::Label(&delete)]);
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
        switch.connect_active_notify({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |switch| {
                if let Some(page) = page.upgrade() {
                    if page.updating.get() {
                        return;
                    }
                    let on = switch.is_active();
                    page.edit(|p| {
                        if on {
                            p.activate(Some(&id));
                        } else if p.active.as_deref() == Some(id.as_str()) {
                            p.activate(None);
                        }
                    });
                }
            }
        });
        row.connect_activated({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.open_editor(&id);
                }
            }
        });
        delete.connect_clicked({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.delete_chain(&id);
                }
            }
        });
        ChainRow {
            row,
            switch,
            delete,
        }
    }

    /// A copy of the active chain under a new name, opened for editing.
    fn new_chain(self: &Rc<Self>) {
        let mut id = String::new();
        self.edit(|p| {
            let steps = p.active().map(|c| c.steps.clone()).unwrap_or_default();
            let name = chains_view::new_name(&p.chains);
            id = p.add(&name, steps);
        });
        self.open_editor(&id);
    }

    fn open_editor(self: &Rc<Self>, id: &str) {
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
        let example = adw::ActionRow::builder()
            .title(gettextrs::gettext("For example"))
            .subtitle_lines(0)
            .css_classes(["property"])
            .build();
        let try_group = adw::PreferencesGroup::new();
        try_group.add(&example);
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
        preferences.add(&name_group);
        preferences.add(&steps);
        preferences.add(&try_group);
        preferences.add(&actions_group);
        let bin = adw::BreakpointBin::builder()
            .width_request(300)
            .height_request(200)
            .child(&preferences)
            .build();
        let breakpoint = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(NARROW_EDITOR).expect("a valid breakpoint condition"),
        );
        bin.add_breakpoint(breakpoint.clone());
        let narrow = Rc::new(Cell::new(false));
        let page = crate::subpage::subpage(&gettextrs::gettext("Chain"), &bin);
        let editor = Rc::new(Editor {
            id: id.to_owned(),
            page: page.clone(),
            name: name.clone(),
            steps,
            add_row: add_row.clone(),
            example,
            use_button: use_button.clone(),
            lines: RefCell::new(Vec::new()),
            shown: RefCell::new(Vec::new()),
            narrow: narrow.clone(),
        });
        for (signal, value) in [("apply", true), ("unapply", false)] {
            let page = Rc::downgrade(self);
            let narrow = narrow.clone();
            breakpoint.connect_local(signal, false, move |_| {
                narrow.set(value);
                if let Some(page) = page.upgrade() {
                    // The pickers change rows: build the steps again.
                    let editor = page.editor.borrow().clone();
                    if let Some(editor) = editor {
                        let count = editor.lines.borrow().len();
                        page.rebuild_lines(&editor, count);
                    }
                    page.render();
                }
                None
            });
        }
        if let Some(chain) = self.deps.store.processing().chain(id) {
            name.set_text(&chain.name);
            page.set_title(&chain.name);
        }
        name.connect_apply({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |entry| {
                if let Some(page) = page.upgrade() {
                    let text = entry.text().trim().to_owned();
                    if !text.is_empty() {
                        page.edit(|p| {
                            p.rename(&id, &text);
                        });
                    }
                }
            }
        });
        add_row.connect_activated({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.open_catalog(Some(&id));
                }
            }
        });
        use_button.connect_clicked({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.edit(|p| {
                        let using = p.active.as_deref() == Some(id.as_str());
                        p.activate((!using).then_some(id.as_str()));
                    });
                }
            }
        });
        delete.connect_clicked({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.delete_chain(&id);
                }
            }
        });
        page.connect_hidden({
            let page = Rc::downgrade(self);
            let id = id.to_owned();
            move |_| {
                if let Some(page) = page.upgrade() {
                    let open = page.editor.borrow().as_ref().map(|e| e.id.clone());
                    if open.as_deref() == Some(id.as_str())
                        && page.deps.nav.visible_page().as_ref() != Some(&page.catalog_page)
                    {
                        page.editor.replace(None);
                    }
                }
            }
        });
        self.editor.replace(Some(editor));
        self.deps.nav.push(&page);
        self.render();
    }

    /// Delete chain `id` and go back; a toast offers it back.
    fn delete_chain(self: &Rc<Self>, id: &str) {
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
        toast.connect_button_clicked({
            let page = Rc::downgrade(self);
            move |_| {
                if let Some(page) = page.upgrade() {
                    let chain = chain.clone();
                    page.edit(|p| {
                        let at = at.min(p.chains.len());
                        if p.chain(&chain.id).is_none() {
                            let id = chain.id.clone();
                            p.chains.insert(at, chain);
                            if was_active {
                                p.activate(Some(&id));
                            }
                        }
                    });
                }
            }
        });
        self.deps.overlay.add_toast(toast);
    }

    fn render_editor(self: &Rc<Self>, processing: &Processing) {
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
        let rows: Vec<Row> = lines
            .iter()
            .map(|line| Row {
                enabled: line.on && !line.skipped,
                ..line.row.clone()
            })
            .collect();
        let shown = example(&rows);
        let result = match (&shown.typed, &shown.note) {
            (Some(typed), _) => format!("\u{201c}{typed}\u{201d}"),
            (None, Some(note)) => format!("({note})"),
            (None, None) => String::new(),
        };
        let text = format!("\u{201c}{}\u{201d}\n\u{2192} {result}", shown.heard);
        editor.example.set_subtitle(&escape_markup(&text));
        editor
            .example
            .update_property(&[gtk::accessible::Property::Description(&text)]);
        editor.shown.replace(lines);
    }

    fn rebuild_lines(self: &Rc<Self>, editor: &Rc<Editor>, count: usize) {
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

    /// Move step `from` of chain `id` to `to`, say so, and keep the focus on
    /// the step that moved.
    fn move_step(self: &Rc<Self>, id: &str, from: usize, to: usize, focus: Focus) {
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
        if let (Some(widgets), Some(line)) = (lines.get(to), shown.get(to)) {
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
    }

    fn line_widgets(self: &Rc<Self>, id: &str, index: usize, narrow: bool) -> LineWidgets {
        let row = adw::ActionRow::builder().subtitle_lines(0).build();
        let number = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .valign(gtk::Align::Center)
            .width_chars(2)
            .build();
        row.add_prefix(&number);
        let badge = gtk::Label::builder()
            .css_classes(["dim-label", "caption", "monospace"])
            .valign(gtk::Align::Center)
            .build();
        let skipped = gtk::Label::builder()
            // TRANSLATORS: marks a step of a chain that does not run where it stands.
            .label(gettextrs::gettext("Skipped"))
            .css_classes(["caption", "skipped-marker"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let picker = gtk::DropDown::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let control = InstallControl::new();
        control.set_visible(false);
        let fix = gtk::Button::builder()
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let up = gtk::Button::builder()
            .icon_name("go-up-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettextrs::gettext("Move up"))
            .build();
        up.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Move up",
        ))]);
        let down = gtk::Button::builder()
            .icon_name("go-down-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .tooltip_text(gettextrs::gettext("Move down"))
            .build();
        down.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
            "Move down",
        ))]);
        let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let item = |label: &str| {
            let button = gtk::Button::builder()
                .label(label)
                .css_classes(["flat"])
                .build();
            menu_box.append(&button);
            button
        };
        let move_up = item(&gettextrs::gettext("Move up"));
        let move_down = item(&gettextrs::gettext("Move down"));
        let to_top = item(&gettextrs::gettext("Move to top"));
        let to_bottom = item(&gettextrs::gettext("Move to bottom"));
        let remove = item(&gettextrs::gettext("Remove step"));
        let remove_model = item(&gettextrs::gettext("Remove model"));
        let popover = gtk::Popover::builder().child(&menu_box).build();
        let menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .popover(&popover)
            .build();
        // The picker sits beside the input it reads, unless the window is
        // too narrow for it and the title both (see `NARROW_EDITOR`).
        let on_row = (!narrow).then_some(picker.upcast_ref::<gtk::Widget>());
        for widget in [
            Some(skipped.upcast_ref::<gtk::Widget>()),
            Some(badge.upcast_ref()),
            on_row,
            Some(switch.upcast_ref()),
            Some(up.upcast_ref()),
            Some(down.upcast_ref()),
            Some(menu.upcast_ref()),
        ]
        .into_iter()
        .flatten()
        {
            row.add_suffix(widget);
        }
        let detail = adw::ActionRow::builder()
            .css_classes(["property"])
            .visible(false)
            .build();
        let under = narrow.then_some(picker.upcast_ref::<gtk::Widget>());
        for widget in [
            Some(fix.upcast_ref::<gtk::Widget>()),
            under,
            Some(control.upcast_ref()),
        ]
        .into_iter()
        .flatten()
        {
            detail.add_suffix(widget);
        }
        let weak = Rc::downgrade(self);
        let chain = id.to_owned();
        let on_page = move |f: PageAction| {
            let weak = weak.clone();
            let chain = chain.clone();
            move || {
                if let Some(page) = weak.upgrade() {
                    f(&page, &chain);
                }
            }
        };
        switch.connect_active_notify({
            let weak = Rc::downgrade(self);
            let chain = id.to_owned();
            move |switch| {
                if let Some(page) = weak.upgrade() {
                    if !page.updating.get() {
                        let on = switch.is_active();
                        page.edit_steps(&chain, |steps| {
                            if let Some(step) = steps.get_mut(index) {
                                step.on = on;
                            }
                        });
                    }
                }
            }
        });
        up.connect_clicked({
            let act = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index.saturating_sub(1), Focus::Up)
            }));
            move |_| act()
        });
        down.connect_clicked({
            let act = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index + 1, Focus::Down)
            }));
            move |_| act()
        });
        let popdown = |popover: &gtk::Popover| {
            let popover = popover.downgrade();
            move || {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            }
        };
        move_up.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index.saturating_sub(1), Focus::Row)
            }));
            move |_| {
                close();
                act()
            }
        });
        move_down.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index + 1, Focus::Row)
            }));
            move |_| {
                close();
                act()
            }
        });
        to_top.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, 0, Focus::Row)
            }));
            move |_| {
                close();
                act()
            }
        });
        to_bottom.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                let last = page
                    .deps
                    .store
                    .processing()
                    .chain(chain)
                    .map_or(0, |c| c.steps.len().saturating_sub(1));
                page.move_step(chain, index, last, Focus::Row)
            }));
            move |_| {
                close();
                act()
            }
        });
        remove.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                page.edit_steps(chain, |steps| {
                    if index < steps.len() {
                        steps.remove(index);
                    }
                });
            }));
            move |_| {
                close();
                act()
            }
        });
        remove_model.connect_clicked({
            let close = popdown(&popover);
            let act = on_page(Box::new(move |page, chain| {
                let step = page
                    .deps
                    .store
                    .processing()
                    .chain(chain)
                    .and_then(|c| c.steps.get(index).map(|s| s.step.clone()));
                if let Some(step) = step {
                    page.remove_step_model(&step);
                }
            }));
            move |_| {
                close();
                act()
            }
        });
        fix.connect_clicked({
            let act = on_page(Box::new(move |page, chain| {
                let above = page
                    .editor
                    .borrow()
                    .as_ref()
                    .and_then(|e| e.shown.borrow().get(index).and_then(|l| l.fix.clone()));
                if let Some((above, _)) = above {
                    page.move_step(chain, index, above, Focus::Row);
                }
            }));
            move |_| act()
        });
        control.button().connect_clicked({
            let act = on_page(Box::new(move |page, chain| page.install_line(chain, index)));
            move |_| act()
        });
        picker.connect_selected_notify({
            let weak = Rc::downgrade(self);
            let chain = id.to_owned();
            move |picker| {
                if let Some(page) = weak.upgrade() {
                    if !page.updating.get() {
                        page.choose(&chain, index, picker.selected());
                    }
                }
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed({
            let act_up = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index.saturating_sub(1), Focus::Row)
            }));
            let act_down = on_page(Box::new(move |page, chain| {
                page.move_step(chain, index, index + 1, Focus::Row)
            }));
            move |_, key, _, modifiers| {
                if !modifiers.contains(gtk::gdk::ModifierType::ALT_MASK) {
                    return glib::Propagation::Proceed;
                }
                match key {
                    gtk::gdk::Key::Up => act_up(),
                    gtk::gdk::Key::Down => act_down(),
                    _ => return glib::Propagation::Proceed,
                }
                glib::Propagation::Stop
            }
        });
        row.add_controller(keys);
        LineWidgets {
            row,
            skipped,
            detail,
            number,
            badge,
            picker,
            listed: RefCell::new(Vec::new()),
            control,
            fix,
            switch,
            up,
            down,
            move_up,
            move_down,
            to_top,
            to_bottom,
            menu,
            remove_model,
        }
    }

    /// The picker of step `index` chose its entry `choice`: a translation's
    /// target (installing it when missing) or a script.
    fn choose(self: &Rc<Self>, id: &str, index: usize, choice: u32) {
        let line = self
            .editor
            .borrow()
            .as_ref()
            .and_then(|e| e.shown.borrow().get(index).cloned());
        let Some(line) = line else { return };
        if let Some(translate) = &line.row.translate {
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
            if !target.installed && target.routed {
                self.install_line(id, index);
            }
        } else if let Some(script) = &line.row.script {
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
    fn install_line(self: &Rc<Self>, id: &str, index: usize) {
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

    /// The catalog for the active chain, opened from elsewhere in Settings
    /// (Diagnostics' Add text model): Install there only installs.
    pub fn open_catalog_to_add(self: &Rc<Self>) {
        self.open_catalog(None);
    }

    fn open_catalog(self: &Rc<Self>, chain: Option<&str>) {
        self.catalog_for.replace(chain.map(str::to_owned));
        self.render();
        self.deps.nav.push(&self.catalog_page);
    }

    /// The chain the catalog adds to now.
    fn catalog_chain(&self, processing: &Processing) -> Option<Chain> {
        match self.catalog_for.borrow().as_deref() {
            Some(id) => processing.chain(id).cloned(),
            None => processing.active().cloned(),
        }
    }

    /// Install `entries` as one operation, shown under `id`.
    fn start_install(self: &Rc<Self>, id: &str, entries: &[Entry]) {
        if self.installing.borrow().contains_key(id) || !self.removing.borrow().is_empty() {
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
                Err(Failure::Failed(error)) => {
                    page.toast_failure(&gettextrs::gettext("Installing the model failed"), &error);
                }
            }
            page.refresh().await;
        });
    }

    fn render_catalog(self: &Rc<Self>, processing: &Processing) {
        let target = self.catalog_chain(processing);
        let flat = target.as_ref().map_or_else(Default::default, |chain| {
            Processing {
                chains: vec![chain.clone()],
                active: Some(chain.id.clone()),
                fit_to_field: false,
            }
            .stage_steps()
        });
        let (ids, params) = flat;
        let offered = self.offered.borrow().clone();
        let source = Source::of_setting(&self.deps.store.language());
        let translate_from = target.as_ref().map_or_else(
            || source.clone(),
            |chain| {
                let at = myna_core::chains::insert_position(&chain.steps, TRANSLATE);
                chains_view::language_at(&chain.steps, at, self.dictation_language().as_deref())
            },
        );
        let catalog = self.catalog.borrow();
        let installing = self.installing.borrow();
        let preparing = self.preparing_now();
        let removing = self.removing.borrow();
        let step_rows = rows(&Inputs {
            language_unset: self.deps.store.language().trim().is_empty(),
            enabled: &ids,
            params: &params,
            source: &source,
            catalog: &catalog,
            daemon: offered.as_ref(),
            installing: &installing,
            preparing: &preparing,
            locales: &self.deps.locales,
        });
        let items = step_catalog::items(&step_catalog::Inputs {
            catalog: &catalog,
            enabled: &ids,
            params: &params,
            source: &source,
            translate_from: &translate_from,
            daemon: offered.as_ref(),
            installing: &installing,
            preparing: &preparing,
            removing: &removing,
            locales: &self.deps.locales,
            rows: &step_rows,
        });
        let likely = self.likely_languages(processing);
        let steps = target.as_ref().map(|c| c.steps.clone()).unwrap_or_default();
        let at = myna_core::chains::insert_position(&steps, TRANSLATE);
        let strands = |code: &str| {
            let mut after = steps.clone();
            after.insert(at, ChainStep::new(TRANSLATE).with("to", code));
            self.view(|ctx| chains_view::strands(&steps, &after, ctx))
        };
        let mut language_installs = BTreeMap::new();
        if let (Some(code), Some(percent)) = (
            self.installing_language.borrow().as_ref(),
            installing.get(TRANSLATE),
        ) {
            language_installs.insert(step_catalog::language_key(code), *percent);
        }
        let languages = step_catalog::languages(&step_catalog::LanguageInputs {
            catalog: &catalog,
            daemon: offered.as_ref(),
            from: &translate_from,
            installing: &language_installs,
            preparing: &BTreeSet::new(),
            likely: &likely,
            strands: &strands,
        });
        let jobs = step_catalog::jobs(&items, &translate_from, &languages);
        let key = |job: &Job| match job {
            Job::Step(item) => item.id.clone(),
            Job::Translate(_) => TRANSLATE.to_owned(),
        };
        let same = {
            let widgets = self.catalog_widgets.borrow();
            widgets.len() == jobs.len()
                && widgets
                    .iter()
                    .zip(&jobs)
                    .all(|((id, _), job)| *id == key(job))
        };
        if !same {
            for (_, widgets) in self.catalog_widgets.take() {
                match widgets {
                    Some(widgets) => self.catalog_group.remove(&widgets.row),
                    None => self.catalog_group.remove(&self.translate_row),
                }
            }
            let mut built = Vec::new();
            for job in &jobs {
                match job {
                    Job::Step(item) => {
                        let widgets = self.catalog_item(item);
                        self.catalog_group.add(&widgets.row);
                        built.push((key(job), Some(widgets)));
                    }
                    Job::Translate(_) => {
                        self.catalog_group.add(&self.translate_row);
                        built.push((key(job), None));
                    }
                }
            }
            self.catalog_widgets.replace(built);
        }
        let blocked = self.dictating.get() || !installing.is_empty() || !removing.is_empty();
        for ((_, widgets), job) in self.catalog_widgets.borrow().iter().zip(&jobs) {
            match (widgets, job) {
                (Some(widgets), Job::Step(item)) => show_item(widgets, item, blocked),
                (None, Job::Translate(translate)) => {
                    self.translate_row
                        .set_subtitle(&escape_markup(&translate.subtitle));
                    self.translate_row.set_sensitive(translate.open);
                }
                _ => {}
            }
        }
        let removable: Vec<Entry> = catalog
            .iter()
            .filter(|e| e.step.id == TRANSLATE)
            .filter(|e| offered.as_ref().is_some_and(|d| d.offers(&e.model)))
            .cloned()
            .collect();
        drop(installing);
        drop(removing);
        drop(catalog);
        self.items.replace(items);
        self.render_languages(&translate_from, languages, &removable, blocked);
    }

    /// The user's own languages, most likely first: the desktop locales,
    /// then the targets of the Translate steps already in a chain.
    fn likely_languages(&self, processing: &Processing) -> Vec<String> {
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

    fn connect_language_page(self: &Rc<Self>) {
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

    fn render_languages(
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
                frame.replace("{language}", &language_name(code))
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
                    .replace("{from}", &language_name(&entry.in_lang))
                    .replace("{to}", &language_name(&entry.out_lang));
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
                remove.update_property(&[gtk::accessible::Property::Label(
                    // TRANSLATORS: {model} names a translation model, such as "English to Français".
                    &gettextrs::gettext("Remove {model}").replace("{model}", &title),
                )]);
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
    fn filter_languages(&self, announce: bool) {
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

    fn language_row(self: &Rc<Self>, code: &str) -> LanguageWidgets {
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
    fn choose_language(self: &Rc<Self>, code: &str) {
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
        let target = self
            .catalog_chain(&self.deps.store.processing())
            .map(|c| c.id);
        if target.is_none() && self.catalog_for.borrow().is_some() {
            self.leave_catalog();
            return;
        }
        let step = ChainStep::new(TRANSLATE).with("to", code);
        let mut placed: Option<(String, usize)> = None;
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
                let at = myna_core::chains::insert_position(&chain.steps, TRANSLATE);
                chain.steps.insert(at, step);
                placed = Some((id, at));
            }
        });
        if let (Some((chain, at)), LanguageState::Download { .. }) = (placed, &language.state) {
            self.installing_language.replace(Some(code.to_owned()));
            self.install_line(&chain, at);
        }
        self.leave_catalog();
    }

    /// Back from the catalog and its language page to where it was opened.
    fn leave_catalog(&self) {
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
    fn add_item(self: &Rc<Self>, id: &str) {
        let Some(item) = self.item(id) else { return };
        if item.offer != Offer::Add {
            return;
        }
        let step = chains_view::step_of(&item, &self.deps.locales);
        let target = self
            .catalog_chain(&self.deps.store.processing())
            .map(|c| c.id);
        // Opened for a chain that is gone since: add to nothing rather than
        // to a chain the user never made.
        if target.is_none() && self.catalog_for.borrow().is_some() {
            self.leave_catalog();
            return;
        }
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
                chain.steps.insert(at, step);
            }
        });
        self.deps.nav.pop();
    }

    fn catalog_item(self: &Rc<Self>, item: &Item) -> CatalogWidgets {
        let row = adw::ExpanderRow::builder()
            .title(escape_markup(&item.title))
            .subtitle_lines(0)
            .build();
        let control = InstallControl::new();
        let add = gtk::Button::builder()
            .label(gettextrs::gettext("Add"))
            .valign(gtk::Align::Center)
            .build();
        // A button is labelled by its label child, over any name set.
        add.reset_relation(gtk::AccessibleRelation::LabelledBy);
        add.update_property(&[gtk::accessible::Property::Label(
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("Add {step}").replace("{step}", &item.title),
        )]);
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
        let mut lines: Vec<(String, String)> = Vec::new();
        if let Some(example) = &item.example {
            lines.push((gettextrs::gettext("For example"), example.clone()));
        }
        lines.extend(
            item.details
                .iter()
                .map(|d| (d.label.clone(), d.text.clone())),
        );
        for (label, text) in lines {
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
        let remove = gtk::Button::builder()
            .label(gettextrs::gettext("Remove"))
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .build();
        remove.reset_relation(gtk::AccessibleRelation::LabelledBy);
        remove.update_property(&[gtk::accessible::Property::Label(
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("Remove {step}").replace("{step}", &item.title),
        )]);
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
        remove.connect_clicked({
            let page = Rc::downgrade(self);
            let id = item.id.clone();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.remove_item(&id);
                }
            }
        });
        add.connect_clicked({
            let page = Rc::downgrade(self);
            let id = item.id.clone();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.add_item(&id);
                }
            }
        });
        control.button().connect_clicked({
            let page = Rc::downgrade(self);
            let id = item.id.clone();
            move |_| {
                if let Some(page) = page.upgrade() {
                    page.install_item(&id);
                }
            }
        });
        CatalogWidgets {
            row,
            control,
            add,
            added,
            remove_row,
            remove,
        }
    }

    fn remove_item(self: &Rc<Self>, id: &str) {
        if let Some(entry) = self.item(id).and_then(|item| item.removable) {
            self.start_remove(*entry);
        }
    }

    fn item(&self, id: &str) -> Option<Item> {
        self.items
            .borrow()
            .iter()
            .find(|item| item.id == id)
            .cloned()
    }

    /// An install or a removal is running.
    fn busy(&self) -> bool {
        !self.installing.borrow().is_empty() || !self.removing.borrow().is_empty()
    }

    fn remove_step_model(self: &Rc<Self>, step: &str) {
        let entry = self
            .items
            .borrow()
            .iter()
            .find(|item| item.step == step && item.removable.is_some())
            .and_then(|item| item.removable.clone());
        if let Some(entry) = entry {
            self.start_remove(*entry);
        }
    }

    /// Remove an installed model. The steps stay as they are: a step whose
    /// model is gone says what it needs and the chain skips it.
    fn start_remove(self: &Rc<Self>, entry: Entry) {
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
            match outcome {
                Ok(()) | Err(Failure::Dismissed) => {}
                Err(Failure::Dictating) => {
                    page.deps
                        .overlay
                        .add_toast(adw::Toast::new(&gettextrs::gettext(
                            "Finish dictating, then try again.",
                        )));
                }
                Err(Failure::Failed(error)) => {
                    page.toast_failure(&gettextrs::gettext("Removing the model failed"), &error);
                }
            }
            page.refresh().await;
        });
    }

    /// The same install as the inline button, for one catalog entry.
    fn install_item(self: &Rc<Self>, id: &str) {
        let Some(item) = self.item(id) else { return };
        let Offer::Install(entry) = item.offer else {
            return;
        };
        if self.dictating.get() || self.busy() {
            return;
        }
        self.start_install(id, std::slice::from_ref(&entry));
    }

    fn toast_failure(&self, heading: &str, error: &crate::ports::SystemConfiguratorError) {
        let heading = heading.to_owned();
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

fn language_page() -> LanguagePage {
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettextrs::gettext("Search languages"))
        .hexpand(true)
        .build();
    search.update_property(&[gtk::accessible::Property::Label(&gettextrs::gettext(
        "Search languages",
    ))]);
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

fn show_language(widgets: &LanguageWidgets, language: &Language, blocked: bool) {
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
    };
    widgets.state.set_label(&state);
    widgets.row.set_sensitive(sensitive);
    let description = match &language.english {
        Some(english) => format!("{english}. {state}"),
        None => state,
    };
    widgets
        .row
        .update_property(&[gtk::accessible::Property::Description(&description)]);
}

fn show_item(widgets: &CatalogWidgets, item: &Item, blocked: bool) {
    widgets.row.set_title(&escape_markup(&item.title));
    let subtitle = match &item.offer {
        Offer::NotApplicable(reason) => reason.clone(),
        Offer::Unknown => gettextrs::gettext("Myna is not running"),
        _ => item.description.clone(),
    };
    widgets.row.set_subtitle(&escape_markup(&subtitle));
    widgets
        .add
        .set_visible(matches!(item.offer, Offer::Add | Offer::NotApplicable(_)));
    widgets.add.set_sensitive(matches!(item.offer, Offer::Add));
    widgets.added.set_visible(item.offer == Offer::Added);
    widgets.remove_row.set_visible(item.removable.is_some());
    widgets.remove.set_sensitive(!blocked);
    let control = &widgets.control;
    control.set_visible(true);
    match &item.offer {
        Offer::Install(_) => {
            control.show_offer(
                &gettextrs::gettext("Install"),
                // TRANSLATORS: {step} is a step's name, such as "Translate English to French".
                &gettextrs::gettext("Install {step}").replace("{step}", &item.title),
            );
            control.button().set_sensitive(!blocked);
        }
        Offer::Installing(percent) => control.show_installing(*percent),
        Offer::Preparing => {
            control.show_installing(None);
            control
                .progress()
                .label
                .set_label(&gettextrs::gettext("Preparing…"));
        }
        Offer::Removing => {
            control.show_installing(None);
            control
                .progress()
                .label
                .set_label(&gettextrs::gettext("Removing…"));
        }
        _ => control.set_visible(false),
    }
}

fn show_standing(control: &InstallControl, standing: &Standing) {
    control.set_visible(true);
    match standing {
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

/// Something a step's widget does to the page, given the chain's id.
type PageAction = Box<dyn Fn(&Rc<StepsPage>, &str)>;

/// Where the focus goes after a step moved.
#[derive(Clone, Copy)]
enum Focus {
    Up,
    Down,
    Row,
}

fn show_line(
    widgets: &LineWidgets,
    line: &Line,
    total: usize,
    blocked: bool,
    narrow: bool,
    items: &[Item],
) {
    widgets.number.set_label(&(line.index + 1).to_string());
    widgets.row.set_title(&escape_markup(&line.title));
    widgets.row.set_subtitle(&escape_markup(&line.subtitle));
    // TRANSLATORS: a step's place in its chain, read by screen readers: "Step 2 of 3".
    let place = gettextrs::gettext("Step {n} of {total}")
        .replace("{n}", &(line.index + 1).to_string())
        .replace("{total}", &total.to_string());
    let description = format!("{place}. {}", line.subtitle);
    widgets
        .row
        .update_property(&[gtk::accessible::Property::Description(&description)]);
    // A skipped step looks inactive, not only says so: dimmed and marked.
    if line.skipped {
        widgets.row.add_css_class("skipped-step");
    } else {
        widgets.row.remove_css_class("skipped-step");
    }
    widgets.skipped.set_visible(line.skipped);
    widgets.badge.set_label(&line.badge);
    widgets.badge.set_visible(!line.badge.is_empty());
    widgets.switch.set_active(line.on);
    widgets
        .switch
        .update_property(&[gtk::accessible::Property::Label(
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("Use {step}").replace("{step}", &line.title),
        )]);
    widgets.up.set_sensitive(line.moves.up);
    widgets.down.set_sensitive(line.moves.down);
    widgets.up.set_visible(!narrow);
    widgets.down.set_visible(!narrow);
    widgets.move_up.set_sensitive(line.moves.up);
    widgets.move_down.set_sensitive(line.moves.down);
    widgets.to_top.set_sensitive(line.moves.top);
    widgets.to_bottom.set_sensitive(line.moves.bottom);
    match &line.fix {
        Some((_, above)) => {
            // TRANSLATORS: a button that moves a skipped step above the step named by {step}.
            widgets
                .fix
                .set_label(&gettextrs::gettext("Move above {step}").replace("{step}", above));
            widgets.fix.set_visible(true);
        }
        None => widgets.fix.set_visible(false),
    }
    let names: Vec<String> = if let Some(translate) = &line.row.translate {
        translate
            .targets
            .iter()
            .map(|target| {
                if target.installed {
                    target.name.clone()
                } else if !target.routed {
                    // TRANSLATORS: {language} is a language name; no model translates into it from the text's language.
                    gettextrs::gettext("{language} (no model)").replace("{language}", &target.name)
                } else {
                    // TRANSLATORS: {language} is a language name; its model is not installed yet.
                    gettextrs::gettext("{language} (not installed)")
                        .replace("{language}", &target.name)
                }
            })
            .collect()
    } else if let Some(script) = &line.row.script {
        script.modes.iter().map(|(_, name)| name.clone()).collect()
    } else {
        Vec::new()
    };
    if *widgets.listed.borrow() != names {
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        widgets.picker.set_model(Some(&gtk::StringList::new(&refs)));
        widgets.listed.replace(names.clone());
    }
    widgets.picker.set_visible(!names.is_empty());
    let selected = line
        .row
        .translate
        .as_ref()
        .map(|t| t.selected)
        .or_else(|| line.row.script.as_ref().map(|s| Some(s.selected)))
        .flatten();
    widgets
        .picker
        .set_selected(selected.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
    // GtkDropDown labels itself by its selected item once one is set, which
    // outranks the label: Orca heard "Simplified (简体)", not what it picks.
    widgets
        .picker
        .reset_relation(gtk::AccessibleRelation::LabelledBy);
    widgets.picker.update_property(
        &[gtk::accessible::Property::Label(&if line.id == TRANSLATE {
            gettextrs::gettext("Translate to")
        } else if line.id == SCRIPT {
            gettextrs::gettext("Write Chinese as")
        } else {
            line.title.clone()
        })],
    );
    let removable = items
        .iter()
        .any(|item| item.step == line.id && item.removable.is_some());
    widgets.remove_model.set_visible(removable);
    widgets.remove_model.set_sensitive(!blocked);
    widgets
        .menu
        .update_property(&[gtk::accessible::Property::Label(
            // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
            &gettextrs::gettext("More actions for {step}").replace("{step}", &line.title),
        )]);
    let installs = line.on
        && !line.skipped
        && matches!(
            line.standing,
            Standing::Missing(_) | Standing::Installing(_) | Standing::Preparing
        );
    if installs {
        show_standing(&widgets.control, &line.standing);
        widgets.control.button().set_sensitive(!blocked);
    } else {
        widgets.control.set_visible(false);
    }
    // From the line and the layout, not the widgets: `is_visible` is false
    // for anything in a hidden row.
    let picker_under = narrow && !names.is_empty();
    let title = if line.fix.is_some() {
        gettextrs::gettext("To make it run")
    } else if picker_under && line.id == TRANSLATE {
        gettextrs::gettext("Translate to")
    } else if picker_under && line.id == SCRIPT {
        gettextrs::gettext("Write Chinese as")
    } else {
        gettextrs::gettext("Model")
    };
    widgets.detail.set_title(&escape_markup(&title));
    widgets
        .detail
        .set_visible(line.fix.is_some() || installs || picker_under);
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
    const BOTH: &str = r#"{"models":[{"model":"m-tidy","task":"text.cleanup"},{"model":"m-fr","task":"text.translate","params":[{"name":"language","values":["en"]},{"name":"to","values":["fr"]}]}],"steps":[]}"#;

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
        async fn change_components(
            &self,
            snap: &str,
            components: &[String],
            install: bool,
            _: CancellationToken,
        ) -> Result<Option<String>, SystemConfiguratorError> {
            let verb = if install { "install" } else { "remove" };
            self.calls
                .borrow_mut()
                .push(format!("{verb} {snap} {}", components.join("+")));
            for _ in 0..4 {
                Nap(false).await;
            }
            if !install {
                self.daemon.replace(NOTHING.to_owned());
            }
            Ok(None)
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
        fit: adw::PreferencesGroup,
        chains: adw::PreferencesGroup,
        more: adw::PreferencesGroup,
        store: Rc<GioClientSettings>,
        calls: Rc<RefCell<Vec<String>>>,
        nav: adw::NavigationView,
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
        myna_core::processing::migrate_steps(&dir.join("keyfile"));
        let source =
            gio::SettingsSchemaSource::from_directory(dir.join("schemas"), None, false).unwrap();
        let store =
            Rc::new(GioClientSettings::open_with_source(&source, dir.join("keyfile")).unwrap());
        let daemon = Rc::new(RefCell::new(daemon.to_owned()));
        let calls = Rc::new(RefCell::new(Vec::new()));
        let group = adw::PreferencesGroup::new();
        let fit = adw::PreferencesGroup::new();
        let more = adw::PreferencesGroup::new();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&fit);
        column.append(&group);
        column.append(&more);
        let nav = adw::NavigationView::new();
        nav.push(
            &adw::NavigationPage::builder()
                .title("Processing")
                .child(&column)
                .build(),
        );
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
                nav: nav.clone(),
            },
            &Slots {
                fit: fit.clone(),
                chains: group.clone(),
                more: more.clone(),
            },
        );
        Rig {
            page,
            fit,
            chains: group,
            more,
            store,
            calls,
            nav,
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

    fn catalog_rows(rig: &Rig) -> Vec<adw::ExpanderRow> {
        all::<adw::ExpanderRow>(rig.page.catalog_group.upcast_ref())
    }

    fn catalog_row(rig: &Rig, title: &str) -> adw::ExpanderRow {
        catalog_rows(rig)
            .into_iter()
            .find(|row| row.title() == title)
            .unwrap_or_else(|| panic!("no catalog row {title}"))
    }

    fn add_button(row: &adw::ExpanderRow) -> gtk::Button {
        all::<gtk::Button>(row.upcast_ref())
            .into_iter()
            .find(|b| b.label().as_deref() == Some("Add"))
            .expect("an Add button")
    }

    fn install_control(row: &impl IsA<gtk::Widget>) -> InstallControl {
        all::<InstallControl>(row.upcast_ref()).remove(0)
    }

    fn chain_row(rig: &Rig, name: &str) -> (adw::ActionRow, gtk::Switch, gtk::Button) {
        let rows = rig.page.chain_rows.borrow();
        let (_, row) = rows
            .iter()
            .find(|(_, row)| row.row.title() == name)
            .unwrap_or_else(|| panic!("no chain row {name}"));
        (row.row.clone(), row.switch.clone(), row.delete.clone())
    }

    fn editor(rig: &Rig) -> Rc<Editor> {
        rig.page.editor.borrow().clone().expect("an open editor")
    }

    fn line_titles(rig: &Rig) -> Vec<String> {
        editor(rig)
            .lines
            .borrow()
            .iter()
            .map(|w| w.row.title().to_string())
            .collect()
    }

    fn steps_of(rig: &Rig, id: &str) -> Vec<(String, bool)> {
        rig.store
            .processing()
            .chain(id)
            .map(|c| c.steps.iter().map(|s| (s.step.clone(), s.on)).collect())
            .unwrap_or_default()
    }

    const TWO: &str = "chains=[('fr', 'French first', [('translate', true, {'to': 'fr'}), ('tidy', true, {})])]\n";

    fn chain_titles(rig: &Rig) -> Vec<String> {
        all::<adw::ActionRow>(rig.chains.upcast_ref())
            .iter()
            .filter(|row| row.is_visible())
            .map(|row| row.title().to_string())
            .collect()
    }

    /// Charles, 2026-11-23: a switch turns a chain on or off without
    /// deleting it, one chain on at a time, and a delete button removes it.
    #[test]
    fn a_chain_is_switched_on_and_off_from_its_row_and_deleted() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "pick",
                TIDY,
                TIDY,
                "chains=[('fr', 'French first', [('translate', true, {'to': 'fr'}), ('tidy', true, {})]), ('t', 'Tidy', [('tidy', true, {})])]\n",
            );
            assert_eq!(chain_titles(&rig), ["French first", "Tidy", "New chain…"]);
            let (row, french, _) = chain_row(&rig, "French first");
            let (_, tidy, _) = chain_row(&rig, "Tidy");
            assert!(
                !french.is_active() && !tidy.is_active(),
                "none on by default"
            );
            assert!(
                row.subtitle().unwrap().contains("(skipped)"),
                "{:?}",
                row.subtitle()
            );
            french.set_active(true);
            assert_eq!(rig.store.processing().active.as_deref(), Some("fr"));
            tidy.set_active(true);
            assert_eq!(rig.store.processing().active.as_deref(), Some("t"));
            assert!(!french.is_active(), "one chain on at a time");
            tidy.set_active(false);
            assert_eq!(rig.store.processing().active, None);
            assert!(
                rig.store.processing().chain("t").is_some(),
                "off is not deleted"
            );
            // A change made elsewhere reaches the page, both ways.
            let mut p = rig.store.processing();
            p.activate(Some("fr"));
            rig.store.set_processing(&p).unwrap();
            pump_until("the external change", || {
                chain_row(&rig, "French first").1.is_active()
            });
            let mut p = rig.store.processing();
            p.activate(None);
            rig.store.set_processing(&p).unwrap();
            pump_until("the external stop", || {
                !chain_row(&rig, "French first").1.is_active()
            });
            row.emit_by_name::<()>("activated", &[]);
            assert_eq!(editor(&rig).id, "fr", "the row opens its editor");
            rig.nav.pop();
            let delete = chain_row(&rig, "French first").2;
            assert_eq!(delete.tooltip_text().unwrap(), "Delete French first");
            delete.emit_clicked();
            assert!(rig.store.processing().chain("fr").is_none());
            assert_eq!(chain_titles(&rig), ["Tidy", "New chain…"]);
        });
    }

    /// Undo puts a deleted chain back where it was, and back in use when it
    /// was the one in use; one that was off comes back off.
    #[test]
    fn undo_puts_a_deleted_chain_back_as_it_was() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "undo",
                TIDY,
                TIDY,
                "chains=[('fr', 'French first', [('translate', true, {'to': 'fr'})]), ('t', 'Tidy', [('tidy', true, {})])]\nactive-chain='t'\n",
            );
            let window = gtk::Window::builder().child(&rig.page.deps.overlay).build();
            window.present();
            // The Undo of the toast that names `name`: an earlier toast can
            // still be on screen while the next one waits behind it.
            let undo = |rig: &Rig, name: &str| {
                let overlay = rig.page.deps.overlay.upcast_ref::<gtk::Widget>().clone();
                let title = format!("{name} deleted");
                let find = || {
                    all::<gtk::Widget>(&overlay)
                        .into_iter()
                        .filter(|w| w.type_().name() == "AdwToastWidget")
                        .find(|toast| all::<gtk::Label>(toast).iter().any(|l| l.label() == title))
                        .and_then(|toast| {
                            all::<gtk::Button>(&toast)
                                .into_iter()
                                .find(|b| b.label().as_deref() == Some("Undo"))
                        })
                };
                pump_until(&title, || find().is_some());
                find().unwrap().emit_clicked();
            };
            chain_row(&rig, "Tidy").2.emit_clicked();
            assert_eq!(rig.store.processing().active, None);
            undo(&rig, "Tidy");
            let p = rig.store.processing();
            assert_eq!(p.active.as_deref(), Some("t"), "back in use");
            assert_eq!(chain_titles(&rig), ["French first", "Tidy", "New chain…"]);

            chain_row(&rig, "French first").2.emit_clicked();
            undo(&rig, "French first");
            let p = rig.store.processing();
            assert!(p.chain("fr").is_some());
            assert_eq!(
                p.active.as_deref(),
                Some("t"),
                "an off chain comes back off"
            );
            window.destroy();
        });
    }

    #[test]
    fn with_no_chain_the_page_says_what_myna_does() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("empty", TIDY, TIDY, "chains=@a(ssa(sba{ss})) []\n");
            let titles = chain_titles(&rig);
            assert_eq!(titles.len(), 2, "{titles:?}");
            assert_eq!(titles[1], "New chain…");
            assert!(rig.page.empty_row.is_visible());
            assert!(
                rig.page.empty_row.subtitle().unwrap().contains("exactly"),
                "{:?}",
                rig.page.empty_row.subtitle()
            );
            rig.page.new_row.emit_by_name::<()>("activated", &[]);
            assert!(!rig.page.empty_row.is_visible());
        });
    }

    #[test]
    fn the_chain_in_use_can_be_stopped_from_its_editor() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("stop", TIDY, TIDY, &format!("{TWO}active-chain='fr'\n"));
            rig.page.open_editor("fr");
            let editor = editor(&rig);
            assert!(editor.use_button.is_sensitive());
            assert_eq!(editor.use_button.label().unwrap(), "Stop using");
            editor.use_button.emit_clicked();
            assert_eq!(rig.store.processing().active, None);
            assert_eq!(editor.use_button.label().unwrap(), "Use this chain");
            assert!(!chain_row(&rig, "French first").1.is_active());
            editor.use_button.emit_clicked();
            assert_eq!(rig.store.processing().active.as_deref(), Some("fr"));
        });
    }

    #[test]
    fn the_fit_switch_reads_and_writes_the_store() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("fit", TIDY, TIDY, "");
            assert!(rig.page.fit_row.is_active(), "on by default");
            rig.page.fit_row.set_active(false);
            assert!(!rig.store.processing().fit_to_field);
            assert!(all::<adw::SwitchRow>(rig.fit.upcast_ref()).len() == 1);
            assert!(all::<adw::ActionRow>(rig.more.upcast_ref())
                .iter()
                .any(|r| r.title() == "Get more steps"));
        });
    }

    #[test]
    fn a_new_chain_copies_the_active_one_and_opens_for_editing() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("new", TIDY, TIDY, &format!("{TWO}active-chain='fr'\n"));
            rig.page.new_row.emit_by_name::<()>("activated", &[]);
            let editor = editor(&rig);
            assert_eq!(rig.nav.visible_page().unwrap(), editor.page);
            let p = rig.store.processing();
            let new = p.chain(&editor.id).unwrap();
            assert_eq!(new.name, "New chain");
            assert_eq!(steps_of(&rig, &editor.id), steps_of(&rig, "fr"));
            assert_eq!(
                p.active.as_deref(),
                Some("fr"),
                "a new chain is not used yet"
            );
            editor.name.set_text("Mine");
            editor.name.emit_by_name::<()>("apply", &[]);
            assert_eq!(
                rig.store.processing().chain(&editor.id).unwrap().name,
                "Mine"
            );
            editor.use_button.emit_clicked();
            assert_eq!(
                rig.store.processing().active.as_deref(),
                Some(editor.id.as_str())
            );
        });
    }

    #[test]
    fn steps_move_switch_off_and_a_skipped_one_offers_its_fix() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("editor", TIDY, TIDY, TWO);
            let (row, _, _) = chain_row(&rig, "French first");
            row.emit_by_name::<()>("activated", &[]);
            let editor = editor(&rig);
            {
                let lines = editor.lines.borrow();
                assert!(!lines[0].up.is_sensitive());
                assert!(!lines[1].down.is_sensitive());
                assert!(
                    lines[1].fix.is_visible(),
                    "tidy after a translation is skipped"
                );
                assert!(lines[1].fix.label().unwrap().contains("Move above"));
                assert!(lines[1].row.subtitle().unwrap().starts_with("Skipped here"));
                // Charles, 2026-11-23: a skipped step looked active.
                assert!(lines[1].row.has_css_class("skipped-step"));
                assert!(lines[1].skipped.is_visible());
                assert!(!lines[1].detail.has_css_class("skipped-step"));
                assert!(!lines[0].row.has_css_class("skipped-step"));
                assert!(!lines[0].skipped.is_visible());
            }
            let fix = editor.lines.borrow()[1].fix.clone();
            fix.emit_clicked();
            assert_eq!(
                steps_of(&rig, "fr"),
                [("tidy".into(), true), ("translate".into(), true)]
            );
            assert!(!editor.lines.borrow()[0].fix.is_visible());
            assert!(!editor.lines.borrow()[0].row.has_css_class("skipped-step"));
            let down = editor.lines.borrow()[0].down.clone();
            down.emit_clicked();
            assert_eq!(steps_of(&rig, "fr")[0].0, "translate");
            let switch = editor.lines.borrow()[0].switch.clone();
            switch.set_active(false);
            assert_eq!(steps_of(&rig, "fr")[0], ("translate".into(), false));
            assert_eq!(line_titles(&rig).len(), 2);
        });
    }

    /// Charles, 2026-11-23: the target belongs on the step's own row, its
    /// input read-only beside it; adcd7307 moved the picker off the row
    /// because at the window's 360 px minimum the title went a letter per
    /// line.
    #[test]
    fn a_step_shows_its_input_and_target_on_its_row_even_at_360_px() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("row", BOTH, BOTH, TWO);
            let window = adw::Window::builder()
                .default_width(360)
                .default_height(700)
                .content(&rig.nav)
                .build();
            window.present();
            rig.page.open_editor("fr");
            let editor = editor(&rig);
            pump_until("the narrow layout", || editor.narrow.get());
            let lines = editor.lines.borrow();
            let translate = &lines[0];
            assert_eq!(translate.badge.label(), "EN \u{2192}");
            assert!(translate.picker.is_visible());
            assert!(
                translate.picker.is_ancestor(&translate.detail),
                "too narrow for the picker beside the title"
            );
            assert!(translate.detail.is_visible());
            assert!(!translate.up.is_visible() && !translate.down.is_visible());
            assert!(translate.move_down.is_sensitive());
            let title = all::<gtk::Label>(translate.row.upcast_ref())
                .into_iter()
                .find(|label| label.has_css_class("title"))
                .expect("the row's title");
            pump_until("the title laid out", || title.height() > 0);
            let (_, one_line, _, _) = title.measure(gtk::Orientation::Vertical, 10_000);
            assert!(
                title.height() <= 2 * one_line,
                "title {}px high, one line is {one_line}px",
                title.height()
            );
            drop(lines);
            window.set_default_size(900, 700);
            pump_until("the wide layout", || !editor.narrow.get());
            let lines = editor.lines.borrow();
            let translate = &lines[0];
            assert!(translate.up.is_visible());
            assert!(translate.picker.is_ancestor(&translate.row));
            assert!(
                !translate.detail.is_visible(),
                "an installed target needs no row under it"
            );
            assert_eq!(translate.badge.label(), "EN \u{2192}");
            drop(lines);
            window.close();
        });
    }

    #[test]
    fn a_step_added_from_the_editor_goes_in_at_its_stage() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "add",
                BOTH,
                BOTH,
                "chains=[('fr', 'French', [('translate', true, {'to': 'fr'})])]\n",
            );
            rig.page.open_editor("fr");
            editor(&rig).add_row.emit_by_name::<()>("activated", &[]);
            assert_eq!(rig.nav.visible_page().unwrap().title(), "Add step");
            let row = catalog_row(&rig, "Tidy up speech");
            pump_until("Add", || {
                add_button(&row).is_visible() && add_button(&row).is_sensitive()
            });
            add_button(&row).emit_clicked();
            assert_eq!(
                steps_of(&rig, "fr"),
                [("tidy".into(), true), ("translate".into(), true)]
            );
            assert_eq!(rig.nav.visible_page().unwrap(), editor(&rig).page);
        });
    }

    /// Charles, 2026-11-23: an empty chain's editor had no Add step row.
    #[test]
    fn adding_to_an_empty_chain_puts_the_step_in_that_chain() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "add-empty",
                TIDY,
                TIDY,
                "chains=[('chain-1', 'New chain', [])]\nactive-chain='chain-1'\n",
            );
            rig.page.open_editor("chain-1");
            let add_row = editor(&rig).add_row.clone();
            assert!(
                add_row.parent().is_some() && add_row.is_visible(),
                "an empty chain shows Add step"
            );
            add_row.emit_by_name::<()>("activated", &[]);
            assert_eq!(rig.nav.visible_page().unwrap(), rig.page.catalog_page);
            let row = catalog_row(&rig, "Tidy up speech");
            pump_until("Add", || {
                add_button(&row).is_visible() && add_button(&row).is_sensitive()
            });
            add_button(&row).emit_clicked();
            let p = rig.store.processing();
            assert_eq!(p.chains.len(), 1, "{:?}", p.chains);
            assert_eq!(steps_of(&rig, "chain-1"), [("tidy".into(), true)]);
        });
    }

    #[test]
    fn adding_for_a_chain_that_is_gone_makes_no_chain() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("add-gone", TIDY, TIDY, "chains=[('fr', 'French', [])]\n");
            rig.page.open_editor("fr");
            rig.page.open_catalog(Some("fr"));
            let row = catalog_row(&rig, "Tidy up speech");
            pump_until("Add", || {
                add_button(&row).is_visible() && add_button(&row).is_sensitive()
            });
            let mut p = rig.store.processing();
            p.delete("fr");
            rig.store.set_processing(&p).unwrap();
            add_button(&row).emit_clicked();
            let p = rig.store.processing();
            assert!(p.chains.is_empty(), "{:?}", p.chains);
            assert_eq!(p.active, None);
        });
    }

    #[test]
    fn adding_with_no_chain_makes_one_and_uses_it() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("add-none", TIDY, TIDY, "");
            rig.page.open_catalog(None);
            let row = catalog_row(&rig, "Tidy up speech");
            pump_until("Add", || {
                add_button(&row).is_visible() && add_button(&row).is_sensitive()
            });
            add_button(&row).emit_clicked();
            let p = rig.store.processing();
            let active = p.active().expect("made and used");
            assert_eq!(active.name, "New chain");
            assert_eq!(active.steps[0].step, "tidy");
        });
    }

    #[test]
    fn a_missing_model_is_installed_from_its_step() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "install",
                NOTHING,
                FRENCH,
                "chains=[('fr', 'French', [('translate', true, {'to': 'fr'})])]\n",
            );
            rig.page.open_editor("fr");
            let row = editor(&rig).lines.borrow()[0].detail.clone();
            pump_until("the Install button", || {
                install_control(&row).is_visible() && install_control(&row).button().is_visible()
            });
            install_control(&row).button().emit_clicked();
            pump_until("the request", || !rig.calls.borrow().is_empty());
            assert_eq!(rig.calls.borrow().as_slice(), ["myna-translation fr"]);
            assert!(editor(&rig).lines.borrow()[0].detail.is_visible());
            assert!(
                !editor(&rig).lines.borrow()[0].row.title().is_empty(),
                "the title keeps its row"
            );
        });
    }

    /// snapd is done but the daemon has not offered the model yet: the step
    /// says it is getting ready, not that it needs an install.
    #[test]
    fn an_installed_model_the_daemon_has_not_offered_is_getting_ready() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "preparing",
                NOTHING,
                NOTHING,
                "chains=[('fr', 'French', [('translate', true, {'to': 'fr'})])]\n",
            );
            rig.page.open_editor("fr");
            let row = editor(&rig).lines.borrow()[0].detail.clone();
            pump_until("the Install button", || {
                install_control(&row).is_visible() && install_control(&row).button().is_visible()
            });
            install_control(&row).button().emit_clicked();
            let says = |rig: &Rig, text: &str| {
                let editor = editor(rig);
                let lines = editor.lines.borrow();
                [
                    lines[0].row.upcast_ref::<gtk::Widget>(),
                    lines[0].detail.upcast_ref(),
                ]
                .iter()
                .flat_map(|w| all::<gtk::Label>(w))
                .any(|l| l.label().contains(text))
            };
            pump_until("getting ready", || says(&rig, "Getting the model ready"));
            assert_eq!(rig.calls.borrow().as_slice(), ["myna-translation fr"]);
        });
    }

    /// The example shows what the chain does now: a step switched off or
    /// skipped where it stands changes nothing in it.
    #[test]
    fn the_example_leaves_out_steps_that_do_not_run() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "example",
                BOTH,
                BOTH,
                "chains=[('fr', 'French', [('translate', false, {'to': 'fr'})])]\n",
            );
            rig.page.open_editor("fr");
            let example = || {
                editor(&rig)
                    .example
                    .subtitle()
                    .unwrap_or_default()
                    .to_string()
            };
            // "“heard”\n→ result": with nothing running, it types what it heard.
            let unchanged = |text: &str| {
                text.split_once("\n\u{2192} ")
                    .is_some_and(|(heard, typed)| heard == typed)
            };
            pump_until("the example", || !example().is_empty());
            assert!(unchanged(&example()), "{}", example());
            let mut p = rig.store.processing();
            p.chains[0].steps[0].on = true;
            rig.store.set_processing(&p).unwrap();
            pump_until("the translated example", || !unchanged(&example()));
        });
    }

    #[test]
    fn deleting_a_chain_goes_back_and_clears_it_if_in_use() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("delete", TIDY, TIDY, &format!("{TWO}active-chain='fr'\n"));
            rig.page.open_editor("fr");
            rig.page.delete_chain("fr");
            let p = rig.store.processing();
            assert!(p.chain("fr").is_none());
            assert_eq!(p.active, None);
            assert!(rig.page.editor.borrow().is_none());
            assert_eq!(rig.nav.visible_page().unwrap().title(), "Processing");
        });
    }

    #[test]
    fn the_catalog_lists_each_job_once_and_no_fit_step() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("catalog", TIDY, TIDY, "");
            rig.page.open_catalog(None);
            let titles: Vec<String> = catalog_rows(&rig)
                .iter()
                .map(|row| row.title().to_string())
                .collect();
            assert_eq!(titles, ["Tidy up speech", "Chinese script"]);
            assert!(rig.page.translate_row.parent().is_some());
            assert_eq!(rig.page.translate_row.title(), "Translate");
            assert!(rig.page.translate_row.is_sensitive());
            assert!(
                rig.page
                    .translate_row
                    .subtitle()
                    .unwrap()
                    .contains("English"),
                "{:?}",
                rig.page.translate_row.subtitle()
            );
        });
    }

    fn language_titles(rig: &Rig) -> Vec<String> {
        rig.page
            .languages
            .rows
            .borrow()
            .iter()
            .filter(|(_, w)| w.row.is_visible())
            .map(|(_, w)| w.row.title().to_string())
            .collect()
    }

    fn open_languages(rig: &Rig, chain: &str) {
        rig.page.open_editor(chain);
        editor(rig).add_row.emit_by_name::<()>("activated", &[]);
        pump_until("the Translate row", || {
            rig.page.translate_row.is_sensitive()
        });
        rig.page.translate_row.emit_by_name::<()>("activated", &[]);
        assert_eq!(rig.nav.visible_page().unwrap(), rig.page.languages.page);
    }

    /// Charles, 2026-11-23: Translate is one job whose page is searched by
    /// language; choosing one adds the step and goes back to the chain.
    #[test]
    fn translate_opens_a_searchable_language_page_and_adds_the_choice() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "languages",
                BOTH,
                BOTH,
                "chains=[('c', 'Mine', [('tidy', true, {})])]\n",
            );
            open_languages(&rig, "c");
            assert_eq!(language_titles(&rig), ["Français", "Deutsch"]);
            rig.page.languages.search.set_text("germ");
            pump_until("the search", || language_titles(&rig) == ["Deutsch"]);
            assert!(!rig.page.languages.empty.is_visible());
            rig.page.languages.search.set_text("klingon");
            pump_until("nothing", || language_titles(&rig).is_empty());
            assert!(rig.page.languages.empty.is_visible());
            rig.page.languages.search.set_text("fran");
            pump_until("French", || language_titles(&rig) == ["Français"]);
            rig.page
                .languages
                .search
                .emit_by_name::<()>("activate", &[]);
            let p = rig.store.processing();
            let steps = &p.chain("c").unwrap().steps;
            assert_eq!(steps.len(), 2);
            assert_eq!(steps[1].step, "translate");
            assert_eq!(steps[1].params.get("to").map(String::as_str), Some("fr"));
            assert_eq!(rig.nav.visible_page().unwrap(), editor(&rig).page);
            assert!(rig.calls.borrow().is_empty(), "French is installed");
            // The installed model is listed to remove.
            assert_eq!(rig.page.languages.installed_rows.borrow().len(), 1);
        });
    }

    /// A language that needs a download waits while another install runs,
    /// dictation or not.
    #[test]
    fn a_download_waits_for_another_install() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig("languages-busy", TIDY, BOTH, "chains=[('c', 'Mine', [])]\n");
            open_languages(&rig, "c");
            pump_until("the download size", || {
                rig.page.languages.rows.borrow()[0]
                    .1
                    .state
                    .label()
                    .starts_with("Downloads")
            });
            rig.page
                .installing
                .borrow_mut()
                .insert("other".into(), None);
            assert!(!rig.page.dictating.get());
            rig.page.choose_language("fr");
            assert!(steps_of(&rig, "c").is_empty(), "nothing added");
            assert!(rig.calls.borrow().is_empty(), "nothing installed");
        });
    }

    #[test]
    fn choosing_a_language_that_is_not_installed_installs_it() {
        crate::ui::on_gtk_thread(|| {
            let rig = rig(
                "languages-install",
                TIDY,
                BOTH,
                "chains=[('c', 'Mine', [])]\n",
            );
            open_languages(&rig, "c");
            // The rig's locale is French: it comes first.
            let code = rig.page.languages.rows.borrow()[0].0.clone();
            assert_eq!(code, "fr");
            pump_until("the download size", || {
                rig.page.languages.rows.borrow()[0]
                    .1
                    .state
                    .label()
                    .starts_with("Downloads")
            });
            rig.page.choose_language("fr");
            assert_eq!(steps_of(&rig, "c"), [("translate".into(), true)]);
            pump_until("the request", || !rig.calls.borrow().is_empty());
            assert_eq!(rig.calls.borrow().as_slice(), ["myna-translation fr"]);
            assert_eq!(rig.nav.visible_page().unwrap(), editor(&rig).page);
        });
    }
}
