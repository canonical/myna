//! Pango markup escaping helper.
//!
//! Any dynamic string routed through a GTK widget property that interprets
//! Pango markup (`AdwActionRow.subtitle`, `AdwPreferencesGroup.description`,
//! `Gtk.Label` with `use-markup: true`, etc.) must be escaped so that
//! backend-provided content — snap names, absolute paths like `<path>` or
//! `ws:<path>`, error messages that contain `&`, `<`, `>` — does not become
//! interpreted markup. Diagnostics `TextView`s are excluded because they show
//! plain text via a `GtkTextBuffer` and do not interpret markup.
//!
//! This module is a thin, well-documented wrapper around
//! [`gtk4::glib::markup_escape_text`] so every call site can be audited by
//! grepping for `escape_markup`.

use gtk4::glib;

/// Escape `input` for safe use as Pango markup or in a widget property that
/// interprets markup. Returns a plain `String` for ergonomics.
pub fn escape_markup(input: &str) -> String {
    glib::markup_escape_text(input).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_ampersand_lt_gt_and_quotes() {
        assert_eq!(escape_markup("a & b"), "a &amp; b");
        assert_eq!(escape_markup("<path>"), "&lt;path&gt;");
        assert_eq!(escape_markup("ws:<path>"), "ws:&lt;path&gt;");
        assert_eq!(escape_markup("\"snap\""), "&quot;snap&quot;");
        assert_eq!(escape_markup("plain text"), "plain text");
    }

    #[test]
    fn escaping_is_idempotent_on_already_safe_content() {
        let once = escape_markup("safe");
        assert_eq!(escape_markup(&once), "safe");
    }
}
