//! What Settings calls each model family Myna knows, keyed by backend snap,
//! and how the recommendation for the user's language orders them.

use std::ops::Range;

use gtk4::glib;
use myna_core::language::{endonym, ModelFamily as Family};

use crate::domain::BackendIdentity;

/// A backend as the model list shows it: a name and, for a family Myna
/// knows, a one-line description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFamily {
    pub name: String,
    pub description: Option<String>,
}

pub fn model_family(snap_name: &str) -> ModelFamily {
    let known = |name: &str, description: String| ModelFamily {
        name: name.to_owned(),
        description: Some(description),
    };
    match Family::from_snap_name(snap_name) {
        Some(Family::Parakeet) => known(
            "Parakeet",
            gettextrs::gettext("Fastest and most accurate in its 25 languages"),
        ),
        Some(Family::Whisper) => known(
            "Whisper",
            gettextrs::gettext("Most languages, with uneven accuracy"),
        ),
        Some(Family::FunAsr) => known(
            "FunASR",
            gettextrs::gettext("Best for Chinese, Japanese and Korean"),
        ),
        Some(Family::IndicConformer) => {
            known("IndicConformer", gettextrs::gettext("Best for Hindi"))
        }
        None => ModelFamily {
            name: title_from_snap(snap_name),
            description: None,
        },
    }
}

/// The known families not installed, the recommended one first, for the
/// install dialog.
pub fn installable_families(
    backends: &[BackendIdentity],
    recommended: Option<Family>,
) -> Vec<Family> {
    let (first, rest): (Vec<Family>, Vec<Family>) = Family::ALL
        .into_iter()
        .filter(|family| {
            !backends
                .iter()
                .any(|backend| backend.snap_name() == family.snap_name())
        })
        .partition(|family| Some(*family) == recommended);
    first.into_iter().chain(rest).collect()
}

/// Where the recommendation for the user's language lands: the pill on
/// General goes to the best installed family, and the best of all, when it
/// is not installed, is what Install more models hints at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recommendation {
    pub installed: Option<Family>,
    pub better: Option<Family>,
}

pub fn recommendation<S: AsRef<str>>(
    preferred: &[S],
    backends: &[BackendIdentity],
) -> Recommendation {
    let installed: Vec<Family> = backends
        .iter()
        .filter_map(|backend| Family::from_snap_name(backend.snap_name()))
        .collect();
    let best = myna_core::language::recommend(preferred);
    Recommendation {
        installed: myna_core::language::recommend_among(preferred, &installed),
        better: (!installed.contains(&best)).then_some(best),
    }
}

/// Whether `backend` belongs to the `recommended` family; a backend of no
/// known family never does.
pub fn is_recommended(backend: &BackendIdentity, recommended: Option<Family>) -> bool {
    recommended.is_some_and(|family| Family::from_snap_name(backend.snap_name()) == Some(family))
}

/// `backends` with the recommended family's first, the rest in order.
pub fn recommended_first(
    backends: &[BackendIdentity],
    recommended: Option<Family>,
) -> Vec<BackendIdentity> {
    let (first, rest): (Vec<&BackendIdentity>, Vec<&BackendIdentity>) = backends
        .iter()
        .partition(|backend| is_recommended(backend, recommended));
    first.into_iter().chain(rest).cloned().collect()
}

/// A language as Settings names it: in itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endonym {
    pub code: &'static str,
    pub name: &'static str,
}

impl Endonym {
    fn of(code: &'static str) -> Option<Self> {
        endonym(code).map(|name| Self { code, name })
    }

    /// The Pango language to shape the name with, so 粵語 takes Traditional
    /// glyph forms whatever the UI language. None for a Latin name, which
    /// would otherwise switch font for the languages fontconfig tags oddly.
    pub fn pango_language(self) -> Option<&'static str> {
        if self
            .name
            .chars()
            .all(|c| !c.is_alphabetic() || is_latin(c) || matches!(c, 'ʻ' | 'ʼ'))
        {
            return None;
        }
        Some(match self.code {
            "yue" => "zh-hk",
            code => code,
        })
    }
}

/// Text that names languages: each name's byte range with its language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub text: String,
    pub names: Vec<(Range<usize>, Endonym)>,
}

impl Named {
    fn plain(text: String) -> Self {
        Self {
            text,
            names: Vec::new(),
        }
    }

    pub fn of(endonym: Endonym) -> Self {
        Self::list(&[endonym], "")
    }

    fn list(endonyms: &[Endonym], separator: &str) -> Self {
        let mut named = Self::plain(String::new());
        for (index, endonym) in endonyms.iter().enumerate() {
            if index > 0 {
                named.text.push_str(separator);
            }
            let start = named.text.len();
            named.text.push_str(endonym.name);
            named.names.push((start..named.text.len(), *endonym));
        }
        named
    }

    /// `frame` with `placeholder` replaced by `self`.
    fn within(self, frame: &str, placeholder: &str) -> Self {
        let Some(offset) = frame.find(placeholder) else {
            return Self::plain(frame.to_owned());
        };
        let text = format!(
            "{}{}{}",
            &frame[..offset],
            self.text,
            &frame[offset + placeholder.len()..]
        );
        let names = self
            .names
            .into_iter()
            .map(|(range, endonym)| (range.start + offset..range.end + offset, endonym))
            .collect();
        Self { text, names }
    }
}

/// A family's languages as its model row shows them, by endonym.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// The user's language first when the family covers it, the rest by name.
    pub languages: Vec<Endonym>,
    /// Whether `languages[0]` is the user's.
    pub user_first: bool,
    /// What the collapsed row names: the user's language, then the most
    /// widely spoken.
    pub summary: Vec<Endonym>,
}

const SUMMARY_LENGTH: usize = 2;

pub fn coverage(family: Family, user_language: Option<&str>) -> Coverage {
    let covered = family.languages();
    let user =
        user_language.and_then(|language| covered.iter().copied().find(|code| *code == language));
    let others = covered
        .iter()
        .copied()
        .filter(|language| Some(*language) != user);
    let summary = user
        .into_iter()
        .chain(others.clone())
        .take(SUMMARY_LENGTH)
        .filter_map(Endonym::of)
        .collect();
    let mut rest: Vec<Endonym> = others.filter_map(Endonym::of).collect();
    rest.sort_by_cached_key(|endonym| (name_order(endonym.name), endonym.name));
    Coverage {
        languages: user.and_then(Endonym::of).into_iter().chain(rest).collect(),
        user_first: user.is_some(),
        summary,
    }
}

/// "English, Deutsch +23".
pub fn coverage_summary(coverage: &Coverage) -> Named {
    // TRANSLATORS: separates language names in a list, as in "English, Deutsch".
    let separator = gettextrs::pgettext("language list", ", ");
    let names = Named::list(&coverage.summary, &separator);
    match coverage
        .languages
        .len()
        .saturating_sub(coverage.summary.len())
    {
        0 => names,
        more => {
            // TRANSLATORS: {languages} is a short list of language names, {count} how many more there are.
            let frame = gettextrs::gettext("{languages} +{count}");
            names.within(&frame.replace("{count}", &more.to_string()), "{languages}")
        }
    }
}

/// "25 languages".
pub fn coverage_count(coverage: &Coverage) -> String {
    let count = coverage.languages.len();
    gettextrs::ngettext("{count} language", "{count} languages", count as u32)
        .replace("{count}", &count.to_string())
}

/// How many columns the full list takes: one for a handful, three for many.
pub fn coverage_columns(count: usize) -> usize {
    match count {
        0..=8 => 1,
        9..=30 => 2,
        _ => 3,
    }
}

/// Where the `index`th of `count` names sits in `columns` columns, filled
/// down each column first so an alphabetical scan reads straight down.
pub fn coverage_cell(index: usize, count: usize, columns: usize) -> (usize, usize) {
    let rows = count.div_ceil(columns.max(1)).max(1);
    (index / rows, index % rows)
}

/// The user's language, when `family` transcribes it and it has a name.
fn covered(family: Family, user_language: Option<&str>) -> Option<Endonym> {
    let language = user_language?;
    family
        .languages()
        .iter()
        .copied()
        .find(|code| *code == language)
        .and_then(Endonym::of)
}

/// The recommended row's pill: why it is recommended, when the family covers
/// the user's language.
pub fn recommendation_label(family: Family, user_language: Option<&str>) -> Named {
    match covered(family, user_language) {
        Some(language) => {
            // TRANSLATORS: {language} is the user's language named in that language, such as "Deutsch" or "中文", inserted as-is in the nominative. If your grammar would inflect it, rephrase, e.g. "Najlepszy dla języka: {language}".
            let frame = gettextrs::gettext("Best for {language}");
            Named::of(language).within(&frame, "{language}")
        }
        None => Named::plain(gettextrs::gettext("Recommended")),
    }
}

/// What Install more models says when `family`, not installed, would serve
/// the user better.
pub fn better_model_hint(family: Family, user_language: Option<&str>) -> Named {
    match covered(family, user_language) {
        Some(language) => {
            // TRANSLATORS: Shown under "Install more models" when a model not installed would transcribe the user's language better. {language} is that language named in itself, such as "Deutsch" or "中文", inserted as-is in the nominative. If your grammar would inflect it, rephrase, e.g. "Dostępny jest lepszy model dla języka: {language}".
            let frame = gettextrs::gettext("A better model for {language} is available");
            Named::of(language).within(&frame, "{language}")
        }
        None => Named::plain(gettextrs::gettext("A recommended model is available")),
    }
}

/// What Install more models says while `family` installs from its dialog,
/// with snapd's download `percent` once known.
pub fn installing_hint(family: Family, percent: Option<u8>) -> Named {
    let name = model_family(family.snap_name()).name;
    let text = match percent {
        // TRANSLATORS: Shown under "Install more models" while a model installs. {model} is a model family, such as "Whisper", and {percent} how much of its download has arrived, a number from 0 to 100.
        Some(percent) => gettextrs::gettext("Installing {model} {percent}%")
            .replace("{percent}", &percent.to_string()),
        // TRANSLATORS: Shown under "Install more models" while a model installs. {model} is a model family, such as "Whisper".
        None => gettextrs::gettext("Installing {model}…"),
    };
    Named::plain(text.replace("{model}", &name))
}

/// Sorts a name by its base letters, so "Čeština" files under C and
/// "ʻŌlelo Hawaiʻi" under O; other scripts follow Latin by code point.
fn name_order(name: &str) -> Vec<char> {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphabetic() && !matches!(c, 'ʻ' | 'ʼ'))
        .map(|c| match c {
            'ə' => 'e',
            'ł' => 'l',
            'ø' => 'o',
            c if is_latin(c) => glib::normalize(c.to_string(), glib::NormalizeMode::Default)
                .chars()
                .next()
                .unwrap_or(c),
            other => other,
        })
        .collect()
}

fn is_latin(c: char) -> bool {
    c.is_ascii_alphabetic()
        || ('\u{c0}'..='\u{24f}').contains(&c)
        || ('\u{1e00}'..='\u{1eff}').contains(&c)
}

/// `myna-fake-backend` reads as "Fake Backend".
fn title_from_snap(snap_name: &str) -> String {
    let stripped = snap_name.strip_prefix("myna-").unwrap_or(snap_name);
    let mut title = String::with_capacity(stripped.len());
    let mut capitalize = true;
    for ch in stripped.chars() {
        if ch == '-' || ch == '_' {
            title.push(' ');
            capitalize = true;
        } else if capitalize {
            title.extend(ch.to_uppercase());
            capitalize = false;
        } else {
            title.push(ch);
        }
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    fn described(snap: &str) -> (String, Option<String>) {
        let family = model_family(snap);
        (family.name, family.description)
    }

    #[test]
    fn known_families_have_a_name_and_a_description() {
        assert_eq!(
            described("myna-parakeet"),
            (
                "Parakeet".to_owned(),
                Some("Fastest and most accurate in its 25 languages".to_owned())
            )
        );
        assert_eq!(
            described("myna-whisper"),
            (
                "Whisper".to_owned(),
                Some("Most languages, with uneven accuracy".to_owned())
            )
        );
        assert_eq!(
            described("myna-funasr"),
            (
                "FunASR".to_owned(),
                Some("Best for Chinese, Japanese and Korean".to_owned())
            )
        );
    }

    #[test]
    fn an_unknown_backend_is_named_after_its_snap_without_a_description() {
        assert_eq!(
            described("myna-fake-backend"),
            ("Fake Backend".to_owned(), None)
        );
        assert_eq!(
            described("myna-whisper-small"),
            ("Whisper Small".to_owned(), None)
        );
        assert_eq!(described("other_asr"), ("Other Asr".to_owned(), None));
        assert_eq!(described("myna-éclair"), ("Éclair".to_owned(), None));
    }

    fn installed(snaps: &[&str]) -> Vec<BackendIdentity> {
        snaps
            .iter()
            .map(|snap| BackendIdentity::new(*snap, "provider"))
            .collect()
    }

    #[test]
    fn every_family_myna_knows_is_offered_until_installed() {
        assert_eq!(installable_families(&[], None), Family::ALL);
    }

    #[test]
    fn the_known_families_not_installed_can_be_installed_recommended_first() {
        assert_eq!(
            installable_families(&installed(&["myna-parakeet"]), None),
            [Family::Whisper, Family::FunAsr, Family::IndicConformer]
        );
        assert_eq!(
            installable_families(&installed(&["myna-parakeet"]), Some(Family::FunAsr)),
            [Family::FunAsr, Family::Whisper, Family::IndicConformer]
        );
        assert_eq!(
            installable_families(&installed(&["myna-whisper"]), Some(Family::Whisper)),
            [Family::Parakeet, Family::FunAsr, Family::IndicConformer],
            "an installed recommendation is not offered again"
        );
        assert_eq!(
            installable_families(&installed(&["myna-fake-backend"]), None),
            [
                Family::Parakeet,
                Family::Whisper,
                Family::FunAsr,
                Family::IndicConformer
            ],
            "a backend of no known family takes no family's place"
        );
        assert_eq!(
            installable_families(
                &installed(&[
                    "myna-funasr",
                    "myna-whisper",
                    "myna-parakeet",
                    "myna-indicconformer"
                ]),
                Some(Family::Parakeet)
            ),
            []
        );
    }

    fn snaps(backends: &[BackendIdentity]) -> Vec<&str> {
        backends.iter().map(BackendIdentity::snap_name).collect()
    }

    #[test]
    fn the_recommended_model_sorts_first_and_the_rest_keep_their_order() {
        let backends = installed(&["myna-fake-backend", "myna-parakeet", "myna-whisper"]);
        assert_eq!(
            snaps(&recommended_first(&backends, Some(Family::Whisper))),
            ["myna-whisper", "myna-fake-backend", "myna-parakeet"]
        );
        assert_eq!(
            snaps(&recommended_first(&backends, Some(Family::Parakeet))),
            ["myna-parakeet", "myna-fake-backend", "myna-whisper"]
        );
        assert_eq!(
            snaps(&recommended_first(&backends, Some(Family::FunAsr))),
            ["myna-fake-backend", "myna-parakeet", "myna-whisper"]
        );
        assert_eq!(
            snaps(&recommended_first(&backends, None)),
            ["myna-fake-backend", "myna-parakeet", "myna-whisper"]
        );
        let unknown_later = installed(&["myna-parakeet", "myna-fake-backend", "myna-whisper"]);
        assert_eq!(
            snaps(&recommended_first(&unknown_later, None)),
            ["myna-parakeet", "myna-fake-backend", "myna-whisper"],
            "no recommendation leaves the order alone"
        );
    }

    fn names(endonyms: &[Endonym]) -> Vec<&'static str> {
        endonyms.iter().map(|endonym| endonym.name).collect()
    }

    /// Each named range of `named` with the text it covers and its language.
    fn spans(named: &Named) -> Vec<(&str, &'static str)> {
        named
            .names
            .iter()
            .map(|(range, endonym)| (&named.text[range.clone()], endonym.code))
            .collect()
    }

    #[test]
    fn the_user_language_leads_the_coverage_and_the_rest_sort_by_name() {
        let english = coverage(Family::Parakeet, Some("en"));
        assert!(english.user_first);
        assert_eq!(english.languages.len(), 25);
        assert_eq!(
            names(&english.languages[..5]),
            ["English", "Čeština", "Dansk", "Deutsch", "Eesti"]
        );
        assert_eq!(
            names(&english.languages[21..]),
            ["Ελληνικά", "Български", "Русский", "Українська"],
            "Latin names first, then other scripts"
        );
        assert_eq!(names(&english.summary), ["English", "Deutsch"]);

        let german = coverage(Family::Parakeet, Some("de"));
        assert_eq!(
            names(&german.languages[..3]),
            ["Deutsch", "Čeština", "Dansk"]
        );
        assert_eq!(names(&german.summary), ["Deutsch", "English"]);

        let chinese = coverage(Family::FunAsr, Some("zh"));
        assert!(chinese.user_first);
        assert_eq!(
            names(&chinese.languages),
            ["中文", "English", "日本語", "粵語", "한국어"]
        );
        assert_eq!(names(&chinese.summary), ["中文", "English"]);
    }

    #[test]
    fn a_language_the_family_lacks_is_not_singled_out() {
        let chinese = coverage(Family::Parakeet, Some("zh"));
        assert!(!chinese.user_first);
        assert_eq!(names(&chinese.languages[..2]), ["Čeština", "Dansk"]);
        assert_eq!(names(&chinese.summary), ["English", "Deutsch"]);
        assert_eq!(coverage(Family::Parakeet, None), chinese);
    }

    #[test]
    fn accents_and_marks_do_not_move_a_name_out_of_its_letter() {
        let whisper = names(&coverage(Family::Whisper, None).languages);
        let position = |name: &str| whisper.iter().position(|n| *n == name).unwrap();
        assert!(position("Hrvatski") < position("Íslenska"));
        assert!(position("Íslenska") < position("Italiano"));
        assert!(position("Nynorsk") < position("ʻŌlelo Hawaiʻi"));
        assert!(position("ʻŌlelo Hawaiʻi") < position("Polski"));
        assert!(position("Română") < position("Shqip"));
        assert!(position("Tiếng Việt") < position("Türkçe"));
        assert!(position("Yorùbá") < position("Ελληνικά"));
    }

    #[test]
    fn every_latin_name_sorts_by_plain_letters() {
        for family in Family::ALL {
            for endonym in family
                .languages()
                .iter()
                .filter_map(|code| Endonym::of(code))
            {
                let letters: Vec<char> = endonym
                    .name
                    .chars()
                    .filter(|c| c.is_alphabetic() && !matches!(c, 'ʻ' | 'ʼ'))
                    .collect();
                if letters.iter().all(|c| is_latin(*c)) {
                    assert!(
                        name_order(endonym.name)
                            .iter()
                            .all(char::is_ascii_lowercase),
                        "{} sorts after z: {:?}",
                        endonym.name,
                        name_order(endonym.name)
                    );
                }
            }
        }
    }

    #[test]
    fn the_collapsed_row_names_two_and_counts_the_rest() {
        let english = coverage_summary(&coverage(Family::Parakeet, Some("en")));
        assert_eq!(english.text, "English, Deutsch +23");
        assert_eq!(spans(&english), [("English", "en"), ("Deutsch", "de")]);
        let japanese = coverage_summary(&coverage(Family::FunAsr, Some("ja")));
        assert_eq!(japanese.text, "日本語, 中文 +3");
        assert_eq!(spans(&japanese), [("日本語", "ja"), ("中文", "zh")]);
        assert_eq!(
            coverage_count(&coverage(Family::Whisper, None)),
            "99 languages"
        );
    }

    #[test]
    fn a_name_is_shaped_as_its_own_language() {
        let pango = |code| Endonym::of(code).unwrap().pango_language();
        assert_eq!(
            pango("yue"),
            Some("zh-hk"),
            "粵語 is written in Traditional"
        );
        assert_eq!(pango("ja"), Some("ja"));
        assert_eq!(pango("zh"), Some("zh"));
        assert_eq!(pango("sr"), Some("sr"));
        assert_eq!(pango("jv"), None, "Basa Jawa keeps the UI font");
        assert_eq!(pango("haw"), None);
    }

    #[test]
    fn the_full_list_fills_down_each_column() {
        assert_eq!(coverage_columns(5), 1);
        assert_eq!(coverage_columns(25), 2);
        assert_eq!(coverage_columns(99), 3);
        assert_eq!(coverage_cell(12, 25, 2), (0, 12));
        assert_eq!(coverage_cell(13, 25, 2), (1, 0));
        assert_eq!(coverage_cell(33, 99, 3), (1, 0));
        assert_eq!(coverage_cell(4, 5, 1), (0, 4));
        for count in 1..=120 {
            let columns = coverage_columns(count);
            let mut cells: Vec<_> = (0..count)
                .map(|index| coverage_cell(index, count, columns))
                .collect();
            assert!(cells.iter().all(|(column, _)| *column < columns), "{count}");
            cells.sort_unstable();
            cells.dedup();
            assert_eq!(cells.len(), count, "{count}");
        }
    }

    #[test]
    fn the_pill_names_the_user_language_the_model_is_best_for() {
        let english = recommendation_label(Family::Parakeet, Some("en"));
        assert_eq!(english.text, "Best for English");
        assert_eq!(spans(&english), [("English", "en")]);
        let chinese = recommendation_label(Family::FunAsr, Some("zh"));
        assert_eq!(chinese.text, "Best for 中文");
        assert_eq!(spans(&chinese), [("中文", "zh")]);
        assert_eq!(
            recommendation_label(Family::Whisper, Some("cy")).text,
            "Best for Cymraeg"
        );
        let norwegian = myna_core::language::user_language(&["nb_NO.UTF-8"]);
        assert_eq!(
            recommendation_label(Family::Whisper, norwegian.as_deref()).text,
            "Best for Norsk"
        );
        let unknown = recommendation_label(Family::Whisper, Some("xx"));
        assert_eq!(unknown.text, "Recommended", "no language to name");
        assert!(unknown.names.is_empty());
        assert_eq!(
            recommendation_label(Family::Whisper, None).text,
            "Recommended"
        );
    }

    #[test]
    fn the_pill_goes_to_the_best_installed_family_and_a_better_one_is_hinted() {
        let recommend = |languages: &[&str], snaps: &[&str]| {
            let Recommendation { installed, better } = recommendation(languages, &installed(snaps));
            (installed, better)
        };
        let whisper_and_parakeet = ["myna-fake-backend", "myna-parakeet", "myna-whisper"];
        for locale in ["zh_CN", "ja_JP", "ko_KR", "yue"] {
            assert_eq!(
                recommend(&[locale], &whisper_and_parakeet),
                (Some(Family::Whisper), Some(Family::FunAsr)),
                "{locale}"
            );
            assert_eq!(
                recommend(&[locale], &["myna-parakeet", "myna-whisper", "myna-funasr"]),
                (Some(Family::FunAsr), None),
                "{locale}"
            );
        }
        assert_eq!(
            recommend(&["en_US"], &whisper_and_parakeet),
            (Some(Family::Parakeet), None)
        );
        assert_eq!(
            recommend(&["de_DE"], &["myna-funasr"]),
            (None, Some(Family::Parakeet)),
            "no installed family transcribes German"
        );
        assert_eq!(
            recommend(&["de_DE"], &["myna-fake-backend"]),
            (None, Some(Family::Parakeet)),
            "a backend of no known family is never recommended"
        );
        assert_eq!(
            recommend(&["de_DE"], &["myna-whisper", "myna-whisper"]),
            (Some(Family::Whisper), Some(Family::Parakeet))
        );
    }

    #[test]
    fn the_hint_names_the_user_language_a_better_model_serves() {
        let chinese = better_model_hint(Family::FunAsr, Some("zh"));
        assert_eq!(chinese.text, "A better model for 中文 is available");
        assert_eq!(spans(&chinese), [("中文", "zh")]);
        assert_eq!(
            better_model_hint(Family::Parakeet, Some("de")).text,
            "A better model for Deutsch is available"
        );
        let unknown = better_model_hint(Family::Whisper, Some("xx"));
        assert_eq!(unknown.text, "A recommended model is available");
        assert!(unknown.names.is_empty());
        assert_eq!(
            better_model_hint(Family::Whisper, None).text,
            "A recommended model is available"
        );
    }

    #[test]
    fn a_backend_of_no_known_family_is_never_recommended() {
        let fake = BackendIdentity::new("myna-fake-backend", "provider");
        let parakeet = BackendIdentity::new("myna-parakeet", "provider");
        assert!(!is_recommended(&fake, None));
        assert!(!is_recommended(&fake, Some(Family::Parakeet)));
        assert!(!is_recommended(&parakeet, None));
        assert!(!is_recommended(&parakeet, Some(Family::Whisper)));
        assert!(is_recommended(&parakeet, Some(Family::Parakeet)));
    }
}
