//! A string-list setting filling a preferences group: an entry row that adds
//! a word, then one row per word with a button that removes it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::myna_settings::{add_list_entry, remove_list_entry, ListLimits, WidgetPlan};

pub struct ListEditor {
    group: adw::PreferencesGroup,
    entry: adw::EntryRow,
    count: gtk::Label,
    rows: RefCell<Vec<adw::ActionRow>>,
    values: RefCell<Vec<String>>,
    limits: Option<ListLimits>,
    writable: bool,
    overlay: adw::ToastOverlay,
    commit: Box<dyn Fn(Vec<String>)>,
}

impl ListEditor {
    /// Fill `group` with the editor. `commit` receives the whole edited
    /// list; the editor shows it once it comes back through
    /// [`ListEditor::apply`].
    pub fn new(
        group: adw::PreferencesGroup,
        plan: &WidgetPlan,
        values: &[String],
        overlay: adw::ToastOverlay,
        commit: Box<dyn Fn(Vec<String>)>,
    ) -> Rc<Self> {
        let count = gtk::Label::builder().valign(gtk::Align::Center).build();
        count.add_css_class("dim-label");
        count.add_css_class("numeric");
        group.set_widget_name(&plan.key);
        group.set_title(&plan.title);
        group.set_description(Some(&plan.description));
        group.set_header_suffix(Some(&count));
        let entry = adw::EntryRow::builder()
            .title(gettextrs::gettext("Add a word or phrase"))
            .sensitive(plan.writable)
            .build();
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text(gettextrs::gettext("Add"))
            .valign(gtk::Align::Center)
            .build();
        add.add_css_class("flat");
        entry.add_suffix(&add);
        group.add(&entry);

        let editor = Rc::new(Self {
            group,
            entry: entry.clone(),
            count,
            rows: RefCell::new(Vec::new()),
            values: RefCell::new(Vec::new()),
            limits: plan.limits,
            writable: plan.writable,
            overlay,
            commit,
        });
        editor.apply(values);
        entry.connect_entry_activated({
            let editor = Rc::downgrade(&editor);
            move |_| add_from_entry(&editor)
        });
        add.connect_clicked({
            let editor = Rc::downgrade(&editor);
            move |_| add_from_entry(&editor)
        });
        entry.connect_changed(|entry| entry.remove_css_class("error"));
        editor
    }

    pub fn apply(self: &Rc<Self>, values: &[String]) {
        for row in self.rows.borrow_mut().drain(..) {
            self.group.remove(&row);
        }
        let rows: Vec<_> = values.iter().map(|value| self.row(value)).collect();
        for row in &rows {
            self.group.add(row);
        }
        *self.rows.borrow_mut() = rows;
        *self.values.borrow_mut() = values.to_vec();
        self.count.set_label(&match self.limits {
            Some(limits) => format!("{} / {}", values.len(), limits.entries),
            None => values.len().to_string(),
        });
    }

    fn row(self: &Rc<Self>, value: &str) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(value)
            .use_markup(false)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(gettextrs::gettext("Remove"))
            .valign(gtk::Align::Center)
            .sensitive(self.writable)
            .build();
        remove.add_css_class("flat");
        remove.update_property(&[gtk::accessible::Property::Label(
            &gettextrs::gettext("Remove {word}").replace("{word}", value),
        )]);
        remove.connect_clicked({
            let editor = Rc::downgrade(self);
            let value = value.to_owned();
            move |_| {
                if let Some(editor) = editor.upgrade() {
                    let values = remove_list_entry(&editor.values.borrow(), &value);
                    (editor.commit)(values);
                }
            }
        });
        row.add_suffix(&remove);
        row
    }
}

fn add_from_entry(editor: &Weak<ListEditor>) {
    let Some(editor) = editor.upgrade() else {
        return;
    };
    let added = add_list_entry(
        &editor.values.borrow(),
        editor.entry.text().as_str(),
        editor.limits,
    );
    match added {
        Ok(values) => {
            editor.entry.set_text("");
            (editor.commit)(values);
        }
        Err(error) => {
            editor.entry.add_css_class("error");
            editor.overlay.add_toast(adw::Toast::new(&error.message()));
        }
    }
}
