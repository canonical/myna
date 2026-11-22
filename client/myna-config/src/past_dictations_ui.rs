//! The Past dictations sub-page of the Processing tab: the "Keep a record"
//! switch on top, then what was recorded, newest first.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::myna_settings::{
    past_dictation, PastDictation, PAST_DICTATIONS_PAGE, PAST_DICTATIONS_READ,
};

/// The widgets of the sub-page. The record switch is added to `record` by the
/// settings builder; the rest follows it.
#[derive(Clone)]
pub struct PastPage {
    page: adw::NavigationPage,
    pub record: adw::PreferencesGroup,
    off: adw::PreferencesGroup,
    list: adw::PreferencesGroup,
}

impl PastPage {
    pub fn new() -> Self {
        let record = adw::PreferencesGroup::new();
        let off = adw::PreferencesGroup::new();
        off.add(
            &adw::StatusPage::builder()
                .icon_name("document-open-recent-symbolic")
                .title(gettextrs::gettext("Not keeping a record"))
                .description(gettextrs::gettext(
                    "Turn on Keep a record and Myna lists here what it heard and typed, newest first. It stays on this computer.",
                ))
                .build(),
        );
        let list = list_group();
        let preferences = adw::PreferencesPage::new();
        preferences.add(&record);
        preferences.add(&off);
        preferences.add(&list);
        let page = adw::NavigationPage::builder()
            .title(gettextrs::gettext("Past dictations"))
            .child(&preferences)
            .build();
        Self {
            page,
            record,
            off,
            list,
        }
    }

    /// The row on the Processing page that opens this sub-page.
    pub fn link_row(&self, nav: &adw::NavigationView) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(gettextrs::gettext("Past dictations"))
            .subtitle(gettextrs::gettext("What Myna heard and typed"))
            .activatable(true)
            .build();
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        row.connect_activated({
            let nav = nav.downgrade();
            let page = self.page.clone();
            move |_| {
                if let Some(nav) = nav.upgrade() {
                    nav.push(&page);
                }
            }
        });
        row
    }

    /// Show the list while the record switch is on, the explanation when off.
    pub fn follow_record(&self, switch: &adw::SwitchRow) {
        switch
            .bind_property("active", &self.list, "visible")
            .sync_create()
            .build();
        switch
            .bind_property("active", &self.off, "visible")
            .sync_create()
            .invert_boolean()
            .build();
    }
}

impl Default for PastPage {
    fn default() -> Self {
        Self::new()
    }
}

/// What the list of recorded dictations is drawn from. The Clear button owns
/// it, so it lives exactly as long as the group it fills.
struct Listing {
    rows: RefCell<Vec<gtk::Widget>>,
    path: Option<std::path::PathBuf>,
    /// How many rows are built; grows by a page at a time.
    shown: Cell<usize>,
    /// Kept so the list follows the daemon as it records.
    monitor: RefCell<Option<gio::FileMonitor>>,
}

fn list_group() -> adw::PreferencesGroup {
    let clear = gtk::Button::builder()
        .label(gettextrs::gettext("Clear"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title(gettextrs::gettext("Recorded"))
        .header_suffix(&clear)
        .build();
    let listing = Rc::new(Listing {
        rows: RefCell::new(Vec::new()),
        path: myna_core::history::path(),
        shown: Cell::new(PAST_DICTATIONS_PAGE),
        monitor: RefCell::new(None),
    });
    if let Some(path) = &listing.path {
        let watched = gio::File::for_path(path)
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE);
        if let Ok(monitor) = watched {
            monitor.connect_changed({
                let listing = Rc::downgrade(&listing);
                let group = group.downgrade();
                move |_, _, _, _| {
                    if let Some((listing, group)) = listing.upgrade().zip(group.upgrade()) {
                        listing.refresh(&listing, &group);
                    }
                }
            });
            listing.monitor.replace(Some(monitor));
        }
    }
    listing.refresh(&listing, &group);
    clear.connect_clicked({
        let group = group.downgrade();
        move |_| {
            if let Some(Err(err)) = listing.path.as_deref().map(myna_core::history::clear) {
                glib::g_warning!(crate::LOG_DOMAIN, "record not cleared: {err}");
            }
            listing.shown.set(PAST_DICTATIONS_PAGE);
            if let Some(group) = group.upgrade() {
                listing.refresh(&listing, &group);
            }
        }
    });
    group
}

impl Listing {
    fn refresh(&self, this: &Rc<Self>, group: &adw::PreferencesGroup) {
        for row in self.rows.take() {
            group.remove(&row);
        }
        let entries = self
            .path
            .as_deref()
            .map(|path| myna_core::history::recent(path, PAST_DICTATIONS_READ))
            .unwrap_or_default();
        let mut rows: Vec<gtk::Widget> = Vec::new();
        if entries.is_empty() {
            rows.push(
                adw::ActionRow::builder()
                    .title(gettextrs::gettext("Nothing recorded yet"))
                    .sensitive(false)
                    .build()
                    .upcast(),
            );
        }
        let shown = self.shown.get();
        rows.extend(entries.iter().take(shown).map(entry_row));
        if entries.len() > shown {
            let more = adw::ActionRow::builder()
                .title(gettextrs::gettext("Show more"))
                .activatable(true)
                .css_classes(["button-row"])
                .build();
            more.add_suffix(&gtk::Image::from_icon_name("view-more-symbolic"));
            more.connect_activated({
                let this = Rc::downgrade(this);
                let group = group.downgrade();
                move |_| {
                    if let Some((this, group)) = this.upgrade().zip(group.upgrade()) {
                        this.shown.set(this.shown.get() + PAST_DICTATIONS_PAGE);
                        this.refresh(&this, &group);
                    }
                }
            });
            rows.push(more.upcast());
        }
        for row in &rows {
            group.add(row);
        }
        self.rows.replace(rows);
    }
}

fn entry_row(entry: &myna_core::history::Entry) -> gtk::Widget {
    let shown = past_dictation(entry);
    let when = i64::try_from(entry.time)
        .ok()
        .and_then(|time| glib::DateTime::from_unix_local(time).ok())
        .and_then(|time| time.format("%-d %b %H:%M").ok())
        .map(|when| {
            gtk::Label::builder()
                .label(when.as_str())
                .css_classes(["dim-label", "caption"])
                .valign(gtk::Align::Center)
                .build()
        });
    let compare = comparison(entry, &shown);
    // A record from before it said what made it has nothing to open.
    if shown.made_by.is_empty() {
        let row = adw::ActionRow::builder()
            .title_lines(0)
            .subtitle_lines(0)
            .build();
        suffixes(&row, when.as_ref(), compare.as_ref());
        wire_compare(&row, &shown, compare);
        return row.upcast();
    }
    let row = adw::ExpanderRow::builder()
        .title_lines(0)
        .subtitle_lines(0)
        .build();
    suffixes(&row, when.as_ref(), compare.as_ref());
    wire_compare(&row, &shown, compare);
    for (label, value) in &shown.made_by {
        row.add_row(
            &adw::ActionRow::builder()
                .title(crate::markup::escape_markup(label))
                .subtitle(crate::markup::escape_markup(value))
                .subtitle_lines(0)
                .subtitle_selectable(true)
                .css_classes(["property"])
                .build(),
        );
    }
    row.upcast()
}

/// The two texts "Original / Result" flips between, when they differ.
struct Comparison {
    toggle: gtk::ToggleButton,
    original: String,
}

fn comparison(entry: &myna_core::history::Entry, shown: &PastDictation) -> Option<Comparison> {
    use myna_core::history::Outcome;
    let landed = matches!(entry.outcome, Some(Outcome::Typed) | None);
    let original = entry.heard.trim();
    if !landed || shown.typed.is_empty() || original == shown.typed || original.is_empty() {
        return None;
    }
    Some(Comparison {
        toggle: gtk::ToggleButton::builder()
            .label(gettextrs::gettext("Original"))
            .tooltip_text(gettextrs::gettext("Show what Myna heard"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build(),
        original: original.to_owned(),
    })
}

/// Suffix widgets of an action or expander row, the button before the time.
trait Suffixes {
    fn put(&self, widget: &gtk::Widget);
}

impl Suffixes for adw::ActionRow {
    fn put(&self, widget: &gtk::Widget) {
        self.add_suffix(widget);
    }
}

impl Suffixes for adw::ExpanderRow {
    fn put(&self, widget: &gtk::Widget) {
        self.add_suffix(widget);
    }
}

fn suffixes(row: &impl Suffixes, when: Option<&gtk::Label>, compare: Option<&Comparison>) {
    if let Some(compare) = compare {
        row.put(compare.toggle.upcast_ref());
    }
    if let Some(when) = when {
        row.put(when.upcast_ref());
    }
}

/// Title and subtitle for the result, and for the original when toggled.
fn wire_compare<R>(row: &R, shown: &PastDictation, compare: Option<Comparison>)
where
    R: Titled + Clone + 'static,
{
    let result = crate::markup::escape_markup(&shown.typed);
    let detail = crate::markup::escape_markup(&shown.detail);
    row.show(&result, &detail);
    let Some(compare) = compare else { return };
    let original = crate::markup::escape_markup(&compare.original);
    let says = crate::markup::escape_markup(&gettextrs::gettext("What Myna heard"));
    compare.toggle.connect_toggled({
        let row = row.clone();
        move |toggle| {
            if toggle.is_active() {
                toggle.set_label(&gettextrs::gettext("Result"));
                toggle.set_tooltip_text(Some(&gettextrs::gettext("Show what Myna typed")));
                row.show(&original, &says);
            } else {
                toggle.set_label(&gettextrs::gettext("Original"));
                toggle.set_tooltip_text(Some(&gettextrs::gettext("Show what Myna heard")));
                row.show(&result, &detail);
            }
        }
    });
}

pub trait Titled {
    fn show(&self, title: &str, subtitle: &str);
}

impl Titled for adw::ActionRow {
    fn show(&self, title: &str, subtitle: &str) {
        self.set_title(title);
        self.set_subtitle(subtitle);
    }
}

impl Titled for adw::ExpanderRow {
    fn show(&self, title: &str, subtitle: &str) {
        self.set_title(title);
        self.set_subtitle(subtitle);
    }
}
