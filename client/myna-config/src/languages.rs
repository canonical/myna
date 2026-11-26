//! The languages Settings names: the one a dictation is in, the catalog's
//! code for a locale, and each language's name in itself and in the
//! interface language.

/// The language a dictation is in, as far as Settings can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// One language, as its bare code.
    One(String),
    /// More than one, or the model decides: translation has no source.
    Unknown,
}

impl Source {
    /// From the `language` setting. Empty is English, as the daemon treats it.
    pub fn of_setting(language: &str) -> Self {
        let language = language.trim();
        if language.is_empty() {
            return Self::One("en".into());
        }
        if language.contains([',', ' ', ';']) || matches!(language, "auto" | "multi") {
            return Self::Unknown;
        }
        Self::One(catalog_code(language))
    }
}

/// The catalog's code for a locale: `fr` for `fr-CA`; `zh-Hans` stays whole,
/// as the models name it, which is why this is not
/// [`myna_core::chains::primary_language`].
pub(crate) fn catalog_code(tag: &str) -> String {
    let tag = tag.replace('_', "-");
    if tag.starts_with("zh-") {
        let mut parts = tag.splitn(3, '-');
        return format!("zh-{}", parts.nth(1).unwrap_or_default());
    }
    tag.split('-')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A language as a person calls it.
pub fn language_name(code: &str) -> String {
    let bare = code.split('-').next().unwrap_or(code);
    match (code, myna_core::language::endonym(bare)) {
        ("zh-Hans", _) => "简体中文".to_owned(),
        ("zh-Hant", _) => "繁體中文".to_owned(),
        (_, Some(name)) => name.to_owned(),
        _ => code.to_owned(),
    }
}

/// A language's name to put inside a sentence ("فارسی to English"): a
/// right-to-left name is isolated (U+2068 ... U+2069), or it would turn
/// the whole sentence around it ("to English فارسی").
pub fn language_in_text(code: &str) -> String {
    let name = language_name(code);
    let rtl = name.chars().any(|c| {
        matches!(c, '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}')
    });
    if rtl {
        format!("\u{2068}{name}\u{2069}")
    } else {
        name
    }
}

/// The English name of every language the catalog offers, by code, each a
/// literal `gettext` call so the extractor sees it. Built once; [`exonym`]
/// reads the names in the interface language, so a search for "German"
/// finds Deutsch.
fn exonyms() -> &'static [(&'static str, String)] {
    static NAMES: std::sync::OnceLock<Vec<(&'static str, String)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        vec![
            ("af", gettextrs::gettext("Afrikaans")),
            ("ar", gettextrs::gettext("Arabic")),
            ("az", gettextrs::gettext("Azerbaijani")),
            ("be", gettextrs::gettext("Belarusian")),
            ("bg", gettextrs::gettext("Bulgarian")),
            ("bn", gettextrs::gettext("Bengali")),
            ("bs", gettextrs::gettext("Bosnian")),
            ("ca", gettextrs::gettext("Catalan")),
            ("cs", gettextrs::gettext("Czech")),
            ("da", gettextrs::gettext("Danish")),
            ("de", gettextrs::gettext("German")),
            ("el", gettextrs::gettext("Greek")),
            ("en", gettextrs::gettext("English")),
            ("es", gettextrs::gettext("Spanish")),
            ("et", gettextrs::gettext("Estonian")),
            ("eu", gettextrs::gettext("Basque")),
            ("fa", gettextrs::gettext("Persian")),
            ("fi", gettextrs::gettext("Finnish")),
            ("fr", gettextrs::gettext("French")),
            ("gl", gettextrs::gettext("Galician")),
            ("gu", gettextrs::gettext("Gujarati")),
            ("he", gettextrs::gettext("Hebrew")),
            ("hi", gettextrs::gettext("Hindi")),
            ("hr", gettextrs::gettext("Croatian")),
            ("hu", gettextrs::gettext("Hungarian")),
            ("id", gettextrs::gettext("Indonesian")),
            ("is", gettextrs::gettext("Icelandic")),
            ("it", gettextrs::gettext("Italian")),
            ("ja", gettextrs::gettext("Japanese")),
            ("kn", gettextrs::gettext("Kannada")),
            ("ko", gettextrs::gettext("Korean")),
            ("lt", gettextrs::gettext("Lithuanian")),
            ("lv", gettextrs::gettext("Latvian")),
            ("ml", gettextrs::gettext("Malayalam")),
            ("mr", gettextrs::gettext("Marathi")),
            ("ms", gettextrs::gettext("Malay")),
            ("nb", gettextrs::gettext("Norwegian Bokmål")),
            ("nl", gettextrs::gettext("Dutch")),
            ("pl", gettextrs::gettext("Polish")),
            ("pt", gettextrs::gettext("Portuguese")),
            ("ro", gettextrs::gettext("Romanian")),
            ("ru", gettextrs::gettext("Russian")),
            ("sk", gettextrs::gettext("Slovak")),
            ("sl", gettextrs::gettext("Slovenian")),
            ("sq", gettextrs::gettext("Albanian")),
            ("sr", gettextrs::gettext("Serbian")),
            ("sv", gettextrs::gettext("Swedish")),
            ("ta", gettextrs::gettext("Tamil")),
            ("te", gettextrs::gettext("Telugu")),
            ("th", gettextrs::gettext("Thai")),
            ("tl", gettextrs::gettext("Tagalog")),
            ("tr", gettextrs::gettext("Turkish")),
            ("ug", gettextrs::gettext("Uyghur")),
            ("uk", gettextrs::gettext("Ukrainian")),
            ("ur", gettextrs::gettext("Urdu")),
            ("vi", gettextrs::gettext("Vietnamese")),
            ("zh", gettextrs::gettext("Chinese")),
            ("zh-Hans", gettextrs::gettext("Chinese (Simplified)")),
            ("zh-Hant", gettextrs::gettext("Chinese (Traditional)")),
        ]
    })
}

/// `code`'s name in the interface language. `None` for a language with no
/// entry; the endonym alone then names it.
pub fn exonym(code: &str) -> Option<String> {
    let (_, name) = exonyms().iter().find(|(have, _)| *have == code)?;
    Some(name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract of the two naming functions: a Chinese target is named
    /// in its own script like every other language, and a language is found
    /// by the name it has in the interface language. Both are what the
    /// language page shows and searches; no display and no catalog needed.
    #[test]
    fn a_language_is_named_where_it_is_listed() {
        assert_eq!(language_name("zh-Hans"), "简体中文");
        assert_eq!(exonym("zh-Hans").as_deref(), Some("Chinese (Simplified)"));
        assert_eq!(language_name("zh-Hant"), "繁體中文");
        assert_eq!(language_in_text("fr"), "Français");
        assert_eq!(language_in_text("fa"), "\u{2068}فارسی\u{2069}");
        assert_eq!(exonym("de").as_deref(), Some("German"));
        assert_eq!(exonym("ja").as_deref(), Some("Japanese"));
        assert_eq!(exonym("zz"), None);
    }
}
