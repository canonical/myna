//! One step's row in the chain editor: its widgets, what they do, and what
//! they show. The decisions are `chains_view` and `processing_steps`; this
//! binds them to widgets, and to the page that owns the chain.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk4::glib;
use libadwaita as adw;

use crate::markup::escape_markup;
use crate::processing_steps::{Moves, Row, Standing, SCRIPT, TRANSLATE};
use crate::step_catalog::LanguageState;
use crate::ui::InstallControl;

use super::processing_ui::{describe, name, Focus, StepsPage};

/// One step in the editor.
pub(crate) struct LineWidgets {
    pub(crate) row: adw::ActionRow,
    /// "Skipped", shown while the text at the step's place does not suit it.
    pub(crate) skipped: gtk::Label,
    /// Under the step, only when it has something to set or do: its
    /// parameter picker, its install and its fix. On the step's own row
    /// they left the title no room.
    pub(crate) detail: adw::ActionRow,
    pub(crate) number: gtk::Label,
    pub(crate) badge: gtk::Label,
    pub(crate) picker: gtk::DropDown,
    /// What the picker lists now, to refill it only when it changes.
    pub(crate) listed: RefCell<Vec<String>>,
    pub(crate) control: InstallControl,
    pub(crate) fix: gtk::Button,
    pub(crate) switch: gtk::Switch,
    /// The ▸/▾ buttons beside the row, which hide below the breakpoint.
    pub(crate) up: gtk::Button,
    pub(crate) down: gtk::Button,
    /// The row menu: the moves it offers, and what it removes.
    pub(crate) moves: Vec<(Move, gtk::Button)>,
    pub(crate) remove: gtk::Button,
    pub(crate) remove_model: gtk::Button,
    pub(crate) menu: gtk::MenuButton,
    pub(crate) popover: gtk::Popover,
}

/// The ▸/▾ buttons beside a step and the menu at its end.
struct RowMenu {
    up: gtk::Button,
    down: gtk::Button,
    popover: gtk::Popover,
    menu: gtk::MenuButton,
    /// The menu's moves, in the order it shows them.
    moves: Vec<(Move, gtk::Button)>,
    remove: gtk::Button,
    remove_model: gtk::Button,
}

/// The row under a step: its fix, its picker when the window is narrow, and
/// its install.
pub(crate) fn line_detail(
    narrow: bool,
    picker: &gtk::DropDown,
) -> (adw::ActionRow, InstallControl, gtk::Button) {
    let control = InstallControl::new();
    control.set_visible(false);
    let fix = gtk::Button::builder()
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    let detail = adw::ActionRow::builder()
        .css_classes(["property"])
        .visible(false)
        .build();
    add_suffixes(
        &detail,
        &[
            Some(fix.upcast_ref()),
            narrow.then_some(picker.upcast_ref()),
            Some(control.upcast_ref()),
        ],
    );
    (detail, control, fix)
}

/// A move of a step, from its row menu or its ▸/▾ buttons.
#[derive(Clone, Copy)]
pub(crate) enum Move {
    Up,
    Down,
    Top,
    Bottom,
}

/// A dimmed, vertically centred label, the shape of a row's own text.
pub(crate) fn dim_label(classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .css_classes(classes)
        .valign(gtk::Align::Center)
        .build()
}

/// Add every widget that is there as a suffix of `row`.
pub(crate) fn add_suffixes(row: &adw::ActionRow, widgets: &[Option<&gtk::Widget>]) {
    for widget in widgets.iter().flatten() {
        row.add_suffix(*widget);
    }
}

/// A flat button of a row menu.
pub(crate) fn menu_item(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .css_classes(["flat"])
        .build()
}

/// A flat icon button, named the same for assistive technology as for the
/// pointer.
pub(crate) fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .css_classes(["flat"])
        .valign(gtk::Align::Center)
        .tooltip_text(label)
        .build();
    name(&button, label);
    button
}

/// Paint a step's row and the row under it.
pub(crate) fn show_line(
    widgets: &LineWidgets,
    line: &Row,
    total: usize,
    blocked: bool,
    narrow: bool,
    items: &[Row],
) {
    show_step(widgets, line, total, narrow);
    let listed = show_parameter(widgets, line);
    show_offers(widgets, line, blocked, narrow, items, listed);
}

/// What the step is and where it stands: its place, its title, its input, the
/// switch that pauses it and the ways to move it.
pub(crate) fn show_step(widgets: &LineWidgets, line: &Row, total: usize, narrow: bool) {
    let place = line.place.index + 1;
    widgets.number.set_label(&place.to_string());
    widgets.row.set_title(&escape_markup(&line.title));
    widgets.row.set_subtitle(&escape_markup(&line.subtitle));
    // TRANSLATORS: a step's place in its chain, read by screen readers: "Step 2 of 3".
    let place = gettextrs::gettext("Step {n} of {total}")
        .replace("{n}", &place.to_string())
        .replace("{total}", &total.to_string());
    describe(&widgets.row, &format!("{place}. {}", line.subtitle));
    // A skipped step looks inactive, not only says so: dimmed and marked.
    if line.place.skipped {
        widgets.row.add_css_class("skipped-step");
    } else {
        widgets.row.remove_css_class("skipped-step");
    }
    widgets.skipped.set_visible(line.place.skipped);
    widgets.badge.set_label(&line.place.badge);
    widgets.badge.set_visible(!line.place.badge.is_empty());
    widgets.switch.set_active(line.on);
    name(
        &widgets.switch,
        // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
        &gettextrs::gettext("Use {step}").replace("{step}", &line.title),
    );
    widgets.up.set_sensitive(line.place.moves.up);
    widgets.down.set_sensitive(line.place.moves.down);
    widgets.up.set_visible(!narrow);
    widgets.down.set_visible(!narrow);
    for (mv, button) in &widgets.moves {
        button.set_sensitive(mv.allowed(line.place.moves));
    }
    name(
        &widgets.menu,
        // TRANSLATORS: {step} is a step's name, such as "Tidy up speech".
        &gettextrs::gettext("More actions for {step}").replace("{step}", &line.title),
    );
    match &line.place.fix {
        Some((_, above)) => {
            // TRANSLATORS: a button that moves a skipped step above the step named by {step}.
            let above = gettextrs::gettext("Move above {step}").replace("{step}", above);
            widgets.fix.set_label(&above);
            widgets.fix.set_visible(true);
        }
        None => widgets.fix.set_visible(false),
    }
}

/// What the step's parameter picks: a translation's target or the Chinese
/// script, listed under the name the picker reads out. Returns whether it
/// lists anything, which the row under the step needs.
pub(crate) fn show_parameter(widgets: &LineWidgets, line: &Row) -> bool {
    let names: Vec<String> = if let Some(translate) = &line.translate {
        translate
            .targets
            .iter()
            .map(|target| match target.state {
                LanguageState::Ready => target.name.clone(),
                LanguageState::NoModel => {
                    // TRANSLATORS: {language} is a language name; no model translates into it from the text's language.
                    gettextrs::gettext("{language} (no model)").replace("{language}", &target.name)
                }
                _ => {
                    // TRANSLATORS: {language} is a language name; its model is not installed yet.
                    gettextrs::gettext("{language} (not installed)")
                        .replace("{language}", &target.name)
                }
            })
            .collect()
    } else if let Some(script) = &line.script {
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
        .translate
        .as_ref()
        .map(|t| t.selected)
        .or_else(|| line.script.as_ref().map(|s| Some(s.selected)))
        .flatten();
    widgets
        .picker
        .set_selected(selected.map_or(gtk::INVALID_LIST_POSITION, |i| i as u32));
    // GtkDropDown labels itself by its selected item once one is set, which
    // outranks the label: Orca heard "Simplified (简体)", not what it picks.
    widgets
        .picker
        .reset_relation(gtk::AccessibleRelation::LabelledBy);
    name(
        &widgets.picker,
        &if line.id == TRANSLATE {
            gettextrs::gettext("Translate to")
        } else if line.id == SCRIPT {
            gettextrs::gettext("Write Chinese as")
        } else {
            line.title.clone()
        },
    );
    !names.is_empty()
}

/// What the step offers besides itself: its install, its model's removal, and
/// the row under it that holds what the narrow layout has no room for.
pub(crate) fn show_offers(
    widgets: &LineWidgets,
    line: &Row,
    blocked: bool,
    narrow: bool,
    items: &[Row],
    listed: bool,
) {
    let removable = items
        .iter()
        .any(|row| row.id == line.id && row.removable.is_some());
    widgets.remove_model.set_visible(removable);
    widgets.remove_model.set_sensitive(!blocked);
    let installs = line.on
        && !line.place.skipped
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
    let picker_under = narrow && listed;
    let title = if line.place.fix.is_some() {
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
        .set_visible(line.place.fix.is_some() || installs || picker_under);
}

impl RowMenu {
    pub(crate) fn new() -> Self {
        let menu_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let popover = gtk::Popover::builder().child(&menu_box).build();
        let menu = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .css_classes(["flat"])
            .valign(gtk::Align::Center)
            .popover(&popover)
            .build();
        let moves: Vec<(Move, gtk::Button)> = Move::MENU
            .iter()
            .map(|&mv| (mv, menu_item(&mv.label())))
            .collect();
        let remove = menu_item(&gettextrs::gettext("Remove step"));
        let remove_model = menu_item(&gettextrs::gettext("Remove model"));
        for button in moves.iter().map(|(_, button)| button) {
            menu_box.append(button);
        }
        menu_box.append(&remove);
        menu_box.append(&remove_model);
        Self {
            up: icon_button("go-up-symbolic", &gettextrs::gettext("Move up")),
            down: icon_button("go-down-symbolic", &gettextrs::gettext("Move down")),
            popover,
            menu,
            moves,
            remove,
            remove_model,
        }
    }
}

impl LineWidgets {
    /// A step's row and the row under it, before anything is wired. Below the
    /// editor's breakpoint the picker moves under the step, so the title keeps
    /// its room beside the badge, the switch and the menu.
    pub(crate) fn new(narrow: bool) -> Self {
        let row = adw::ActionRow::builder().subtitle_lines(0).build();
        let number = dim_label(&["dim-label", "numeric"]);
        number.set_width_chars(2);
        row.add_prefix(&number);
        let badge = dim_label(&["dim-label", "caption", "monospace"]);
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
        let switch = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let RowMenu {
            up,
            down,
            popover,
            menu,
            moves,
            remove,
            remove_model,
        } = RowMenu::new();
        add_suffixes(
            &row,
            &[
                Some(skipped.upcast_ref()),
                Some(badge.upcast_ref()),
                (!narrow).then_some(picker.upcast_ref()),
            ],
        );
        add_suffixes(
            &row,
            &[
                Some(switch.upcast_ref()),
                Some(up.upcast_ref()),
                Some(down.upcast_ref()),
                Some(menu.upcast_ref()),
            ],
        );
        let (detail, control, fix) = line_detail(narrow, &picker);
        Self {
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
            moves,
            remove,
            remove_model,
            menu,
            popover,
        }
    }
}

impl Move {
    /// The row menu's entries, in the order it shows them.
    const MENU: [Self; 4] = [Self::Up, Self::Down, Self::Top, Self::Bottom];

    /// What its menu entry says.
    pub(crate) fn label(self) -> String {
        match self {
            Self::Up => gettextrs::gettext("Move up"),
            Self::Down => gettextrs::gettext("Move down"),
            Self::Top => gettextrs::gettext("Move to top"),
            Self::Bottom => gettextrs::gettext("Move to bottom"),
        }
    }

    /// Whether it keeps every translation of the chain doable.
    pub(crate) fn allowed(self, moves: Moves) -> bool {
        match self {
            Self::Up => moves.up,
            Self::Down => moves.down,
            Self::Top => moves.top,
            Self::Bottom => moves.bottom,
        }
    }

    /// Where the step at `from` of `len` goes, if it goes anywhere.
    pub(crate) fn target(self, from: usize, len: usize) -> Option<usize> {
        let to = match self {
            Self::Up => from.checked_sub(1)?,
            Self::Down => from + 1,
            Self::Top => 0,
            Self::Bottom => len.checked_sub(1)?,
        };
        (to != from && to < len).then_some(to)
    }
}

pub(crate) fn show_standing(control: &InstallControl, standing: &Standing) {
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
impl StepsPage {
    /// A step in the editor: its row, the row under it, and what they do.
    pub(crate) fn line_widgets(
        self: &Rc<Self>,
        id: &str,
        index: usize,
        narrow: bool,
    ) -> LineWidgets {
        let widgets = LineWidgets::new(narrow);
        self.wire_line(&widgets, id, index);
        widgets
    }

    /// Give step `index`'s controls what they do.
    pub(crate) fn wire_line(self: &Rc<Self>, widgets: &LineWidgets, id: &str, index: usize) {
        widgets
            .switch
            .connect_active_notify(self.on(id, move |page, id, switch: &gtk::Switch| {
                let on = switch.is_active();
                page.edit_steps(id, |steps| {
                    if let Some(step) = steps.get_mut(index) {
                        step.on = on;
                    }
                });
            }));
        widgets
            .up
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                page.move_by(id, index, Move::Up, Focus::Up);
            }));
        widgets
            .down
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                page.move_by(id, index, Move::Down, Focus::Down);
            }));
        self.wire_menu(widgets, id, index);
        widgets
            .fix
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                let above = page.editor.borrow().as_ref().and_then(|e| {
                    e.shown
                        .borrow()
                        .get(index)
                        .and_then(|l| l.place.fix.clone())
                });
                if let Some((above, _)) = above {
                    page.move_step(id, index, above, Focus::Row);
                }
            }));
        widgets
            .control
            .button()
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                page.install_line(id, index);
            }));
        widgets.picker.connect_selected_notify(
            self.on(id, move |page, id, picker: &gtk::DropDown| {
                page.choose(id, index, picker.selected())
            }),
        );
        let up = self.on(id, move |page, id, _: &gtk::EventControllerKey| {
            page.move_by(id, index, Move::Up, Focus::Row);
        });
        let down = self.on(id, move |page, id, _: &gtk::EventControllerKey| {
            page.move_by(id, index, Move::Down, Focus::Row);
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |keys, key, _, modifiers| {
            if !modifiers.contains(gtk::gdk::ModifierType::ALT_MASK) {
                return glib::Propagation::Proceed;
            }
            match key {
                gtk::gdk::Key::Up => up(keys),
                gtk::gdk::Key::Down => down(keys),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        widgets.row.add_controller(keys);
    }

    /// What a step's row menu does: the moves, the step's removal and its
    /// model's.
    pub(crate) fn wire_menu(self: &Rc<Self>, widgets: &LineWidgets, id: &str, index: usize) {
        for (mv, button) in &widgets.moves {
            let mv = *mv;
            let popover = widgets.popover.clone();
            button.connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                popover.popdown();
                page.move_by(id, index, mv, Focus::Row);
            }));
        }
        let popover = widgets.popover.clone();
        widgets
            .remove
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                popover.popdown();
                page.edit_steps(id, |steps| {
                    if index < steps.len() {
                        steps.remove(index);
                    }
                });
            }));
        let popover = widgets.popover.clone();
        widgets
            .remove_model
            .connect_clicked(self.on(id, move |page, id, _: &gtk::Button| {
                popover.popdown();
                let step = page
                    .deps
                    .store
                    .processing()
                    .chain(id)
                    .and_then(|c| c.steps.get(index).map(|s| s.step.clone()));
                if let Some(step) = step {
                    page.remove_step_model(&step);
                }
            }));
    }
}
