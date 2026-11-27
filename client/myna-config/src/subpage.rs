//! The one way a sub-page is made, so each shows its way back.

use gtk4 as gtk;
use libadwaita as adw;
use libadwaita::prelude::*;

/// A navigation page holding `content` under a header bar. The window's own
/// header holds the tab switcher, so this bar carries only the back button.
pub fn subpage(title: &str, content: &impl IsA<gtk::Widget>) -> adw::NavigationPage {
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(content));
    adw::NavigationPage::builder()
        .title(title)
        .child(&toolbar)
        .build()
}
