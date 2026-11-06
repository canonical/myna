//! Which model family transcribes a language best, from the published model
//! cards, and the recommendation Settings shows for the user's language.

use crate::locale::language_code;

/// A backend family Myna knows, whatever its snap currently ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModelFamily {
    Parakeet,
    Whisper,
    FunAsr,
}

impl ModelFamily {
    /// Every family, in the order Settings lists them.
    pub const ALL: [ModelFamily; 3] = [Self::Parakeet, Self::Whisper, Self::FunAsr];

    pub fn snap_name(self) -> &'static str {
        match self {
            Self::Parakeet => "myna-parakeet",
            Self::FunAsr => "myna-funasr",
            Self::Whisper => "myna-whisper",
        }
    }

    pub fn from_snap_name(snap_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|family| family.snap_name() == snap_name)
    }

    /// The languages the family transcribes, as its card lists them, the
    /// most widely spoken first.
    pub fn languages(self) -> &'static [&'static str] {
        match self {
            Self::Parakeet => PARAKEET_LANGUAGES,
            Self::Whisper => WHISPER_LANGUAGES,
            Self::FunAsr => FUNASR_LANGUAGES,
        }
    }

    /// Whether the backend decodes in the spoken-language setting. Parakeet
    /// and SenseVoice detect the language themselves and ignore it.
    pub fn takes_spoken_language(self) -> bool {
        self == Self::Whisper
    }

    /// The `streaming` value the family's snap installs with.
    pub fn streams_by_default(self) -> bool {
        self == Self::Parakeet
    }
}

/// The parakeet-tdt-0.6b-v3 card's 25 languages.
const PARAKEET_LANGUAGES: &[&str] = &[
    "en", "de", "fr", "es", "it", "pt", "ru", "pl", "uk", "nl", "ro", "cs", "el", "hu", "sv", "bg",
    "da", "fi", "sk", "hr", "lt", "sl", "lv", "et", "mt",
];

/// SenseVoice-Small, the model myna-funasr ships.
const FUNASR_LANGUAGES: &[&str] = &["zh", "en", "yue", "ja", "ko"];

/// Whisper's 99 languages in its tokenizer's order, which follows the
/// training data. Whisper's own code for Javanese is `jw`; this is ISO `jv`.
const WHISPER_LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jv", "su",
];

const ENDONYMS: &[(&str, &str)] = &[
    ("af", "Afrikaans"),
    ("am", "አማርኛ"),
    ("ar", "العربية"),
    ("as", "অসমীয়া"),
    ("az", "Azərbaycan"),
    ("ba", "Башҡорт"),
    ("be", "Беларуская"),
    ("bg", "Български"),
    ("bn", "বাংলা"),
    ("bo", "བོད་སྐད་"),
    ("br", "Brezhoneg"),
    ("bs", "Bosanski"),
    ("ca", "Català"),
    ("cs", "Čeština"),
    ("cy", "Cymraeg"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("el", "Ελληνικά"),
    ("en", "English"),
    ("es", "Español"),
    ("et", "Eesti"),
    ("eu", "Euskara"),
    ("fa", "فارسی"),
    ("fi", "Suomi"),
    ("fo", "Føroyskt"),
    ("fr", "Français"),
    ("gl", "Galego"),
    ("gu", "ગુજરાતી"),
    ("ha", "Hausa"),
    ("haw", "ʻŌlelo Hawaiʻi"),
    ("he", "עברית"),
    ("hi", "हिन्दी"),
    ("hr", "Hrvatski"),
    ("ht", "Kreyòl ayisyen"),
    ("hu", "Magyar"),
    ("hy", "Հայերեն"),
    ("id", "Bahasa Indonesia"),
    ("is", "Íslenska"),
    ("it", "Italiano"),
    ("ja", "日本語"),
    ("jv", "Basa Jawa"),
    ("ka", "ქართული"),
    ("kk", "Қазақ тілі"),
    ("km", "ខ្មែរ"),
    ("kn", "ಕನ್ನಡ"),
    ("ko", "한국어"),
    ("la", "Latina"),
    ("lb", "Lëtzebuergesch"),
    ("ln", "Lingála"),
    ("lo", "ລາວ"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("mg", "Malagasy"),
    ("mi", "Māori"),
    ("mk", "Македонски"),
    ("ml", "മലയാളം"),
    ("mn", "Монгол"),
    ("mr", "मराठी"),
    ("ms", "Bahasa Melayu"),
    ("mt", "Malti"),
    ("my", "မြန်မာ"),
    ("ne", "नेपाली"),
    ("nl", "Nederlands"),
    ("nn", "Nynorsk"),
    ("no", "Norsk"),
    ("oc", "Occitan"),
    ("pa", "ਪੰਜਾਬੀ"),
    ("pl", "Polski"),
    ("ps", "پښتو"),
    ("pt", "Português"),
    ("ro", "Română"),
    ("ru", "Русский"),
    ("sa", "संस्कृतम्"),
    ("sd", "سنڌي"),
    ("si", "සිංහල"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("sn", "ChiShona"),
    ("so", "Soomaali"),
    ("sq", "Shqip"),
    ("sr", "Српски"),
    ("su", "Basa Sunda"),
    ("sv", "Svenska"),
    ("sw", "Kiswahili"),
    ("ta", "தமிழ்"),
    ("te", "తెలుగు"),
    ("tg", "Тоҷикӣ"),
    ("th", "ไทย"),
    ("tk", "Türkmen"),
    ("tl", "Tagalog"),
    ("tr", "Türkçe"),
    ("tt", "Татар"),
    ("uk", "Українська"),
    ("ur", "اردو"),
    ("uz", "Oʻzbek"),
    ("vi", "Tiếng Việt"),
    ("yi", "ייִדיש"),
    ("yo", "Yorùbá"),
    ("yue", "粵語"),
    ("zh", "中文"),
];

/// `language`'s name in itself, capitalised as a language chooser lists it.
pub fn endonym(language: &str) -> Option<&'static str> {
    ENDONYMS
        .iter()
        .find(|(code, _)| *code == language)
        .map(|(_, name)| *name)
}

/// Locale language codes the model cards spell otherwise: glibc's Bokmål
/// and Filipino, and the pre-1989 ISO codes some locale aliases still use.
const ALIASES: &[(&str, &str)] = &[
    ("nb", "no"),
    ("fil", "tl"),
    ("iw", "he"),
    ("in", "id"),
    ("ji", "yi"),
];

/// The bare code of the most preferred language, the one the recommendation
/// is for, spelled as the model cards spell it.
pub fn user_language<S: AsRef<str>>(preferred: &[S]) -> Option<String> {
    let language = preferred
        .iter()
        .find_map(|locale| language_code(locale.as_ref()))?;
    Some(
        ALIASES
            .iter()
            .find(|(alias, _)| *alias == language)
            .map_or(language, |(_, code)| (*code).to_owned()),
    )
}

/// A published error rate (WER or CER, percent) of one family on one
/// language. Rates are ordinal and cross-dataset even within a language, so
/// they only order families; the margin means nothing.
struct Measured {
    language: &'static str,
    family: ModelFamily,
    error_rate: f32,
}

const fn measured(language: &'static str, family: ModelFamily, error_rate: f32) -> Measured {
    Measured {
        language,
        family,
        error_rate,
    }
}

use ModelFamily::{FunAsr, Parakeet, Whisper};

/// Parakeet: nvidia/parakeet-tdt-0.6b-v3 card, FLEURS WER, its full language
/// list. FunASR: SenseVoice-Small against Whisper-small, Common Voice, from
/// the SenseVoice paper (arXiv 2407.04051) table 6. Whisper covers every
/// other language unmeasured.
const TABLE: &[Measured] = &[
    measured("bg", Parakeet, 12.64),
    measured("cs", Parakeet, 11.01),
    measured("da", Parakeet, 18.41),
    measured("de", Parakeet, 5.04),
    measured("el", Parakeet, 20.70),
    measured("en", Parakeet, 4.85),
    measured("es", Parakeet, 3.45),
    measured("et", Parakeet, 17.73),
    measured("fi", Parakeet, 13.21),
    measured("fr", Parakeet, 5.15),
    measured("hr", Parakeet, 12.46),
    measured("hu", Parakeet, 15.72),
    measured("it", Parakeet, 3.00),
    measured("lt", Parakeet, 20.35),
    measured("lv", Parakeet, 22.84),
    measured("mt", Parakeet, 20.46),
    measured("nl", Parakeet, 7.48),
    measured("pl", Parakeet, 7.31),
    measured("pt", Parakeet, 4.76),
    measured("ro", Parakeet, 12.44),
    measured("ru", Parakeet, 5.51),
    measured("sk", Parakeet, 8.82),
    measured("sl", Parakeet, 24.03),
    measured("sv", Parakeet, 15.08),
    measured("uk", Parakeet, 6.79),
    measured("en", FunAsr, 14.71),
    measured("ja", FunAsr, 11.96),
    measured("ko", FunAsr, 8.28),
    measured("yue", FunAsr, 7.09),
    measured("zh", FunAsr, 10.78),
    measured("en", Whisper, 14.85),
    measured("ja", Whisper, 19.51),
    measured("ko", Whisper, 10.48),
    measured("yue", Whisper, 38.97),
    measured("zh", Whisper, 19.60),
];

/// The family to recommend for `preferred` languages, most preferred first,
/// out of every family Myna knows.
pub fn recommend<S: AsRef<str>>(preferred: &[S]) -> ModelFamily {
    recommend_among(preferred, &ModelFamily::ALL).unwrap_or(Whisper)
}

/// The family among `candidates` to recommend for `preferred` languages,
/// or `None` when no candidate can transcribe the most preferred one.
/// Settings passes the installed families to choose among what is there.
///
/// Only the first language decides: the rest are fallbacks the user settled
/// for, and a Welsh speaker listing English after Welsh still speaks Welsh.
pub fn recommend_among<S: AsRef<str>>(
    preferred: &[S],
    candidates: &[ModelFamily],
) -> Option<ModelFamily> {
    let language = user_language(preferred);
    let measured = language.as_deref().and_then(|language| {
        TABLE
            .iter()
            .filter(|entry| entry.language == language && candidates.contains(&entry.family))
            .min_by(|a, b| a.error_rate.total_cmp(&b.error_rate))
            .map(|entry| entry.family)
    });
    measured.or_else(|| candidates.contains(&Whisper).then_some(Whisper))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whisper_takes_the_spoken_language() {
        let takers: Vec<ModelFamily> = ModelFamily::ALL
            .into_iter()
            .filter(|family| family.takes_spoken_language())
            .collect();
        assert_eq!(takers, [ModelFamily::Whisper]);
    }

    #[test]
    fn european_languages_go_to_parakeet_whatever_the_region() {
        for locale in [
            "pt_BR",
            "pt_PT",
            "de_CH.UTF-8",
            "en_US",
            "en_GB.UTF-8",
            "fr_CA",
            "uk_UA",
        ] {
            assert_eq!(recommend(&[locale]), Parakeet, "{locale}");
        }
    }

    #[test]
    fn chinese_japanese_and_korean_go_to_funasr_whatever_the_region() {
        for locale in [
            "zh_CN.UTF-8",
            "zh_TW",
            "zh_HK",
            "zh_SG",
            "ja_JP",
            "ko_KR.UTF-8",
            "yue",
        ] {
            assert_eq!(recommend(&[locale]), FunAsr, "{locale}");
        }
    }

    #[test]
    fn a_language_no_card_measures_falls_back_to_whisper() {
        for locale in ["sr_RS", "cy_GB", "ar_EG.UTF-8", "hi_IN", "tr_TR", "xx"] {
            assert_eq!(recommend(&[locale]), Whisper, "{locale}");
        }
    }

    #[test]
    fn nothing_to_go_on_falls_back_to_whisper() {
        assert_eq!(recommend::<&str>(&[]), Whisper);
        assert_eq!(recommend(&["C", "POSIX", ""]), Whisper);
    }

    #[test]
    fn only_the_most_preferred_language_decides() {
        assert_eq!(recommend(&["cy_GB", "en_GB", "en"]), Whisper);
        assert_eq!(recommend(&["ja_JP", "en"]), FunAsr);
        assert_eq!(recommend(&["en_GB", "ja"]), Parakeet);
    }

    #[test]
    fn locale_names_that_are_not_languages_are_skipped() {
        assert_eq!(recommend(&["C", "ja_JP"]), FunAsr);
        assert_eq!(recommend(&["POSIX", "C.UTF-8", "de"]), Parakeet);
    }

    #[test]
    fn the_lowest_error_rate_breaks_a_tie_between_candidates() {
        assert_eq!(recommend_among(&["en"], &[Whisper, FunAsr]), Some(FunAsr));
        assert_eq!(recommend_among(&["en"], &[FunAsr, Whisper]), Some(FunAsr));
        assert_eq!(
            recommend_among(&["en_US"], &ModelFamily::ALL),
            Some(Parakeet)
        );
        assert_eq!(recommend_among(&["ko"], &[Whisper, FunAsr]), Some(FunAsr));
    }

    #[test]
    fn without_the_best_family_the_next_measured_one_wins() {
        assert_eq!(recommend_among(&["zh_TW"], &[Whisper]), Some(Whisper));
        assert_eq!(recommend_among(&["de"], &[FunAsr, Whisper]), Some(Whisper));
    }

    #[test]
    fn among_the_installed_the_best_measured_one_is_recommended() {
        for locale in ["zh_CN", "ja_JP", "ko_KR", "yue"] {
            assert_eq!(
                recommend_among(&[locale], &[Parakeet, Whisper]),
                Some(Whisper),
                "{locale}"
            );
            assert_eq!(
                recommend_among(&[locale], &[Parakeet, Whisper, FunAsr]),
                Some(FunAsr),
                "{locale}"
            );
        }
        assert_eq!(recommend_among(&["de_DE"], &[Whisper]), Some(Whisper));
    }

    #[test]
    fn no_candidate_for_the_language_recommends_nothing() {
        assert_eq!(recommend_among(&["de"], &[FunAsr]), None);
        assert_eq!(recommend_among(&["ja"], &[Parakeet]), None);
        assert_eq!(recommend_among(&["en"], &[]), None);
        assert_eq!(recommend_among::<&str>(&[], &[Parakeet]), None);
        assert_eq!(
            recommend_among::<&str>(&[], &[Parakeet, Whisper]),
            Some(Whisper)
        );
    }

    #[test]
    fn every_measured_language_is_a_bare_lowercase_code() {
        for entry in TABLE {
            assert_eq!(
                language_code(entry.language).as_deref(),
                Some(entry.language)
            );
            assert!(entry.error_rate > 0.0 && entry.error_rate < 100.0);
        }
    }

    #[test]
    fn each_family_covers_what_its_card_lists() {
        assert_eq!(Parakeet.languages().len(), 25);
        assert_eq!(FunAsr.languages().len(), 5);
        assert_eq!(Whisper.languages().len(), 99);
        for family in [Parakeet, FunAsr] {
            let mut measured: Vec<&str> = TABLE
                .iter()
                .filter(|entry| entry.family == family)
                .map(|entry| entry.language)
                .collect();
            let mut covered = family.languages().to_vec();
            measured.sort_unstable();
            covered.sort_unstable();
            assert_eq!(covered, measured, "{family:?}");
        }
    }

    #[test]
    fn every_covered_language_has_one_endonym() {
        for family in ModelFamily::ALL {
            let languages = family.languages();
            for (index, language) in languages.iter().enumerate() {
                assert!(endonym(language).is_some(), "{language}");
                assert_eq!(
                    language_code(language).as_deref(),
                    Some(*language),
                    "{language}"
                );
                assert!(
                    !languages[..index].contains(language),
                    "{family:?} lists {language} twice"
                );
            }
        }
        for (index, (language, name)) in ENDONYMS.iter().enumerate() {
            assert!(
                ENDONYMS[..index].iter().all(|(other, _)| other != language),
                "{language}"
            );
            assert_eq!(name.trim(), *name);
            assert!(!name.is_empty());
        }
    }

    #[test]
    fn languages_are_named_in_themselves() {
        assert_eq!(endonym("en"), Some("English"));
        assert_eq!(endonym("de"), Some("Deutsch"));
        assert_eq!(endonym("fr"), Some("Français"));
        assert_eq!(endonym("zh"), Some("中文"));
        assert_eq!(endonym("yue"), Some("粵語"));
        assert_eq!(endonym("ja"), Some("日本語"));
        assert_eq!(endonym("xx"), None);
        assert_eq!(endonym("de_DE"), None, "a bare code, not a locale");
    }

    #[test]
    fn the_user_language_is_the_most_preferred_one_bare() {
        assert_eq!(user_language(&["de_CH.UTF-8", "en"]).as_deref(), Some("de"));
        assert_eq!(user_language(&["C", "zh_TW"]).as_deref(), Some("zh"));
        assert_eq!(user_language(&["C", "POSIX"]), None);
        assert_eq!(user_language::<&str>(&[]), None);
    }

    #[test]
    fn locale_codes_resolve_to_the_code_the_models_list() {
        for (locale, language) in [
            ("nb_NO.UTF-8", "no"),
            ("fil_PH", "tl"),
            ("iw_IL", "he"),
            ("in_ID", "id"),
            ("ji_US", "yi"),
        ] {
            assert_eq!(user_language(&[locale]).as_deref(), Some(language));
            assert!(Whisper.languages().contains(&language), "{language}");
        }
        assert_eq!(user_language(&["nn_NO"]).as_deref(), Some("nn"));
    }

    #[test]
    fn families_are_named_by_their_snap() {
        for family in ModelFamily::ALL {
            assert_eq!(
                ModelFamily::from_snap_name(family.snap_name()),
                Some(family)
            );
        }
        assert_eq!(ModelFamily::from_snap_name("myna-fake-backend"), None);
        assert_eq!(Parakeet.snap_name(), "myna-parakeet");
        assert_eq!(FunAsr.snap_name(), "myna-funasr");
        assert_eq!(Whisper.snap_name(), "myna-whisper");
    }
}
