//! Regression tests for [`myna_config::markup::escape_markup`]. Escaping is
//! the single choke point for backend-provided content that ends up in
//! Pango markup–interpreting widget properties (subtitles, descriptions,
//! `use-markup: true` labels).

use myna_config::markup::escape_markup;

#[test]
fn escapes_path_placeholder_and_ws_prefixed_path() {
    assert_eq!(escape_markup("<path>"), "&lt;path&gt;");
    assert_eq!(escape_markup("ws:<path>"), "ws:&lt;path&gt;");
}

#[test]
fn escapes_ampersand_in_backend_message() {
    assert_eq!(
        escape_markup("connect failed & retry blocked"),
        "connect failed &amp; retry blocked"
    );
}

#[test]
fn preserves_plain_text_verbatim() {
    for value in [
        "myna-parakeet",
        "Connected",
        "Refreshing…",
        "engine=cpu backend=onnx",
        "Multiple backends are connected.",
    ] {
        assert_eq!(escape_markup(value), value);
    }
}

#[test]
fn hostile_pango_markup_becomes_literal_text() {
    let hostile = "<span foreground='red'>bold</span> &amp;";
    let escaped = escape_markup(hostile);
    // Angle brackets and the raw ampersand were both escaped.
    assert!(escaped.starts_with("&lt;span"));
    assert!(escaped.contains("&amp;amp;"));
    assert!(!escaped.contains("<span"));
}

/// Widget-level regression: exercise the exact hostile shapes that motivated
/// the escape audit — filesystem paths as `<path>`, `ws:<path>` websocket
/// references, raw ampersands from backend errors, and markup-like backend
/// strings. Every dynamic subtitle/description at the audited call sites in
/// `backend_ui.rs` funnels through [`escape_markup`], so this covers those
/// call sites without instantiating GTK widgets.
#[test]
fn widget_level_hostile_subtitles_and_descriptions() {
    let cases = [
        ("<path>", "&lt;path&gt;"),
        ("ws:<path>", "ws:&lt;path&gt;"),
        (
            "connect failed & readback denied",
            "connect failed &amp; readback denied",
        ),
        ("<b>myna-parakeet</b>", "&lt;b&gt;myna-parakeet&lt;/b&gt;"),
        (
            "engine=onnx & backend=<invalid>",
            "engine=onnx &amp; backend=&lt;invalid&gt;",
        ),
        (
            // A backend-provided surface error surfaced into a description.
            "modelctl exited 1: /var/snap/myna-x/data missing",
            "modelctl exited 1: /var/snap/myna-x/data missing",
        ),
        (
            // An entrypoint value that includes ampersand-quoted attributes.
            "cmd=/snap/bin/myna & args=--k=v",
            "cmd=/snap/bin/myna &amp; args=--k=v",
        ),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            escape_markup(raw),
            expected,
            "escape_markup({raw:?}) did not match"
        );
    }
}

#[test]
fn escaped_strings_are_stable_when_reencoded_by_gtk() {
    // A dynamic subtitle that already went through escape_markup should
    // remain safe if it is copied into a second markup-enabled property.
    let once = escape_markup("<snap> & </snap>");
    let twice = escape_markup(&once);
    assert!(twice.contains("&amp;lt;snap&amp;gt;"));
    assert!(!twice.contains("<snap>"));
}
