//! The user's vocabulary applied to text: `spoken => written` entries and
//! multi-word terms by longest match, and near-misses of long single words
//! by sound.
//!
//! Sound is a small consonant skeleton of our own (`sound`), not
//! `rphonetic`: its `enum-iterator` dependency is 0BSD, which `deny.toml`
//! does not allow.
//!
//! An entry is `written` (a word or term to spell as given) or
//! `spoken => written` (what the speech model tends to hear, then what to
//! type). Matching is ASCII case-insensitive and stops at word edges.

use aho_corasick::{AhoCorasick, Input, MatchKind};

/// Shortest word, in characters, that may be taken for a vocabulary word it
/// only sounds like: shorter ones are too often another real word.
const NEAR_MIN_CHARS: usize = 6;

/// Most single edits (insert, delete, substitute, swap) a near-miss may be.
const NEAR_MAX_EDITS: usize = 1;

/// What an entry is heard as and what it is typed as.
fn split(entry: &str) -> (&str, &str) {
    myna_core::spoken_written(entry).unwrap_or((entry.trim(), entry.trim()))
}

/// What `entry` is typed as, and so what the speech model is told to listen
/// for.
pub fn written(entry: &str) -> &str {
    split(entry).1
}

/// The words a speech model is told to listen for.
pub fn keywords(entries: &[String]) -> Vec<String> {
    entries.iter().map(|e| written(e).to_owned()).collect()
}

/// A single word long enough to be matched by sound.
struct Near {
    lower: String,
    code: String,
    written: String,
}

#[derive(Default)]
pub struct Vocabulary {
    phrases: Option<AhoCorasick>,
    /// What each phrase is typed as, in pattern order.
    typed: Vec<String>,
    near: Vec<Near>,
}

impl Vocabulary {
    pub fn new(entries: &[String]) -> Self {
        let mut patterns = Vec::new();
        let mut typed = Vec::new();
        let mut near: Vec<Near> = Vec::new();
        for entry in entries {
            let (spoken, written) = split(entry);
            let pattern = spoken
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            if pattern.is_empty() {
                continue;
            }
            patterns.push(pattern);
            typed.push(written.to_owned());
            let lower = written.to_lowercase();
            let single = lower.chars().all(char::is_alphanumeric);
            if single && lower.chars().count() >= NEAR_MIN_CHARS {
                let code = sound(&lower);
                near.push(Near {
                    lower,
                    code,
                    written: written.to_owned(),
                });
            }
        }
        let phrases = (!patterns.is_empty())
            .then(|| {
                AhoCorasick::builder()
                    .match_kind(MatchKind::LeftmostLongest)
                    .ascii_case_insensitive(true)
                    .build(&patterns)
                    .ok()
            })
            .flatten();
        Self {
            phrases,
            typed,
            near,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.phrases.is_none()
    }

    /// `text` with its vocabulary spelled as the user wrote it.
    pub fn apply(&self, text: &str) -> String {
        let Some(phrases) = &self.phrases else {
            return text.to_owned();
        };
        let mut edits = self.phrase_edits(phrases, text);
        edits.extend(self.near_edits(text, &edits));
        edits.sort_by_key(|(start, ..)| *start);
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (start, end, replacement) in edits {
            out.push_str(&text[at..start]);
            out.push_str(&replacement);
            at = end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// Longest matches at word edges, left to right, as
    /// `(start, end, replacement)`; a match that already reads as written
    /// is no edit but still holds its place.
    fn phrase_edits(&self, phrases: &AhoCorasick, text: &str) -> Vec<(usize, usize, String)> {
        let mut edits = Vec::new();
        let mut from = 0;
        while let Some(found) = phrases.find(Input::new(text).span(from..text.len())) {
            let (start, end) = (found.start(), found.end());
            if at_word_edges(text, start, end) {
                let seen = &text[start..end];
                let typed = &self.typed[found.pattern().as_usize()];
                edits.push((start, end, restyle(seen, typed)));
                from = end;
            } else {
                from = start + text[start..].chars().next().map_or(1, char::len_utf8);
            }
        }
        edits
    }

    /// Long words one edit from a vocabulary word and sounding like it,
    /// outside the spans already `taken`. A word two vocabulary words fit
    /// is left alone.
    fn near_edits(
        &self,
        text: &str,
        taken: &[(usize, usize, String)],
    ) -> Vec<(usize, usize, String)> {
        if self.near.is_empty() {
            return Vec::new();
        }
        let mut edits = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices().chain([(text.len(), ' ')]) {
            if c.is_alphanumeric() {
                start.get_or_insert(i);
                continue;
            }
            let Some(s) = start.take() else { continue };
            let word = &text[s..i];
            if word.chars().count() < NEAR_MIN_CHARS
                || taken.iter().any(|(a, b, _)| (*a..*b).contains(&s))
            {
                continue;
            }
            let lower = word.to_lowercase();
            let code = sound(&lower);
            let mut fits = self.near.iter().filter(|n| {
                n.lower != lower
                    && n.code == code
                    && strsim::damerau_levenshtein(&n.lower, &lower) <= NEAR_MAX_EDITS
            });
            if let (Some(only), None) = (fits.next(), fits.next()) {
                edits.push((s, i, restyle(word, &only.written)));
            }
        }
        edits
    }
}

/// The consonants of a lowercase word, with the likes of `c`/`k`/`q`,
/// `s`/`z`, `f`/`v`/`ph` as one and doubled letters as one. Vowels count
/// only at the start, so a vowel typo keeps the key and a consonant typo
/// changes it. Good enough to tell a misspelt term from another word.
fn sound(word: &str) -> String {
    let word = word
        .replace("ph", "f")
        .replace("ck", "k")
        .replace("kn", "n")
        .replace("wr", "r");
    let letters: Vec<char> = word.chars().collect();
    let mut key = String::new();
    for (i, &c) in letters.iter().enumerate() {
        let next = letters.get(i + 1).copied();
        let mapped = match c {
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' if i == 0 => 'a',
            'a' | 'e' | 'i' | 'o' | 'u' | 'y' | 'w' | 'h' => continue,
            'c' if matches!(next, Some('e' | 'i' | 'y')) => 's',
            'c' | 'q' => 'k',
            'z' => 's',
            'v' => 'f',
            'x' => {
                key.push('k');
                's'
            }
            other => other,
        };
        if !key.ends_with(mapped) {
            key.push(mapped);
        }
    }
    key
}

/// Neither neighbour of `start..end` is part of a word.
fn at_word_edges(text: &str, start: usize, end: usize) -> bool {
    !text[..start]
        .chars()
        .next_back()
        .is_some_and(char::is_alphanumeric)
        && !text[end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
}

/// `typed` for the `seen` it replaces. A lowercase word the user wrote
/// keeps the capital or capitals of what was heard, so a vocabulary of
/// `kernel` neither lowercases a sentence's first word nor shouts.
fn restyle(seen: &str, typed: &str) -> String {
    if typed.chars().any(char::is_uppercase) {
        return typed.to_owned();
    }
    if seen.to_lowercase() == typed {
        return seen.to_owned();
    }
    let mut chars = typed.chars();
    match (
        seen.chars().next().is_some_and(char::is_uppercase),
        chars.next(),
    ) {
        (true, Some(first)) => first.to_uppercase().chain(chars).collect(),
        _ => typed.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(entries: &[&str]) -> Vocabulary {
        Vocabulary::new(&entries.iter().map(|e| (*e).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn a_spoken_form_is_typed_as_the_user_wrote_it() {
        let v = vocab(&["my sequel => MySQL"]);
        assert_eq!(v.apply("I use my sequel daily."), "I use MySQL daily.");
        assert_eq!(v.apply("My Sequel is up"), "MySQL is up");
    }

    #[test]
    fn the_longest_entry_wins() {
        let v = vocab(&["sequel => SQL", "my sequel => MySQL"]);
        assert_eq!(v.apply("my sequel and sequel"), "MySQL and SQL");
    }

    #[test]
    fn only_whole_words_match() {
        let v = vocab(&["sequel => SQL"]);
        assert_eq!(v.apply("a sequels sequel"), "a sequels SQL");
        assert_eq!(v.apply("presequel"), "presequel");
        assert_eq!(v.apply("sequel's end"), "SQL's end");
    }

    #[test]
    fn a_match_hidden_behind_a_partial_one_is_still_found() {
        let v = vocab(&["abab => X", "ba => Y"]);
        assert_eq!(v.apply("aba ba"), "aba Y");
    }

    #[test]
    fn a_plain_entry_recases_what_was_heard() {
        let v = vocab(&["MySQL", "Siobhan"]);
        assert_eq!(v.apply("mysql and siobhan"), "MySQL and Siobhan");
    }

    #[test]
    fn a_lowercase_entry_never_flattens_a_sentence_start() {
        let v = vocab(&["kernel"]);
        assert_eq!(
            v.apply("Kernel panic. the kernel. KERNEL"),
            "Kernel panic. the kernel. KERNEL"
        );
        let v = vocab(&["sequel => sql"]);
        assert_eq!(
            v.apply("Sequel is fine, sequel too"),
            "Sql is fine, sql too"
        );
    }

    #[test]
    fn a_long_word_one_edit_away_and_alike_is_corrected() {
        let v = vocab(&["Kubernetes", "Siobhan"]);
        assert_eq!(v.apply("we run Kubernettes now"), "we run Kubernetes now");
        assert_eq!(v.apply("Kubernetis"), "Kubernetes");
        assert_eq!(v.apply("ask Siobhann"), "ask Siobhan");
    }

    #[test]
    fn a_short_or_distant_word_is_left_alone() {
        let v = vocab(&["Kubernetes", "Kent", "Siobhan"]);
        assert_eq!(v.apply("Kemt went"), "Kemt went");
        assert_eq!(v.apply("Kubernxxes"), "Kubernxxes");
        assert_eq!(v.apply("Siobhaan"), "Siobhan", "one insert is within reach");
        assert_eq!(v.apply("Siobhaaan"), "Siobhaaan", "two are not");
    }

    #[test]
    fn the_sound_of_a_word_is_its_consonants() {
        assert_eq!(sound("kubernetes"), sound("kubernetis"));
        assert_eq!(sound("siobhan"), sound("siobhaan"));
        assert_eq!(sound("phone"), sound("fone"));
        assert_eq!(sound("cell"), sound("sell"));
        assert_eq!(sound("quick"), sound("kwik"));
        assert_eq!(sound("knight"), sound("night"));
        assert_eq!(sound("write"), sound("rite"));
        assert_eq!(sound("clock"), sound("clok"));
        assert_eq!(sound("vase"), sound("fase"));
        assert_eq!(sound("zeal"), sound("seal"));
        assert_eq!(sound("axe"), sound("aks"));
        assert_eq!(sound("matter"), sound("mater"));
        assert_ne!(sound("problem"), sound("problems"));
        assert_ne!(sound("problem"), sound("probtem"));
        assert_ne!(sound("kernel"), sound("kernal-x"));
        assert_ne!(sound("ace"), sound("ice-k"));
        assert_eq!(sound("ace"), sound("ice"), "a leading vowel is any vowel");
        assert_eq!(sound("echo"), sound("oko"));
        assert_ne!(sound("echo"), sound("cho"), "a leading vowel is a sound");
    }

    #[test]
    fn a_word_is_long_enough_from_six_characters() {
        let v = vocab(&["Marcos"]);
        assert_eq!(v.apply("Marcoz"), "Marcos", "six is long enough");
        assert_eq!(v.apply("Marco"), "Marco", "five is not");
        let v = vocab(&["Marco"]);
        assert_eq!(v.apply("Marcoo"), "Marcoo", "nor is a five-letter term");
    }

    #[test]
    fn a_word_beside_a_match_is_still_corrected() {
        let v = vocab(&["go => GO", "Kubernetes"]);
        assert_eq!(v.apply("go kubernettes"), "GO Kubernetes");
        assert_eq!(v.apply("kubernettes go"), "Kubernetes GO");
    }

    #[test]
    fn a_word_inside_a_match_is_not_corrected_again() {
        let v = vocab(&["kubernettes => K8s", "Kubernetes"]);
        assert_eq!(v.apply("kubernettes"), "K8s");
    }

    #[test]
    fn a_swap_of_letters_counts_as_one_edit() {
        let v = vocab(&["Kubernetes"]);
        assert_eq!(v.apply("Kubernetse"), "Kubernetes");
    }

    #[test]
    fn a_word_that_only_looks_alike_is_left_alone() {
        // One edit away but not the same sound: Metaphone codes differ.
        let v = vocab(&["problem"]);
        assert_eq!(v.apply("problems"), "problems");
        assert_eq!(v.apply("probtem"), "probtem");
    }

    #[test]
    fn a_word_two_entries_fit_is_not_guessed() {
        let v = vocab(&["Kubernetes", "Kubernetis"]);
        assert_eq!(v.apply("Kubernetas"), "Kubernetas");
    }

    #[test]
    fn a_word_already_matched_is_not_corrected_again() {
        let v = vocab(&["kubernettes => Kubernetes", "Kubernites"]);
        assert_eq!(v.apply("kubernettes"), "Kubernetes");
    }

    #[test]
    fn a_term_with_a_colon_is_plain() {
        for term in ["ratio:", ":x", "a:b", "localhost:8080", "std::vector"] {
            assert_eq!(written(term), term);
        }
        assert_eq!(written(" my sequel => MySQL "), "MySQL");
        let v = vocab(&["localhost:8080"]);
        assert_eq!(v.apply("open localhost"), "open localhost");
    }

    #[test]
    fn the_speech_model_is_told_the_written_forms() {
        assert_eq!(
            keywords(&["my sequel => MySQL".into(), "Siobhan".into()]),
            ["MySQL", "Siobhan"]
        );
    }

    #[test]
    fn an_empty_vocabulary_changes_nothing() {
        assert!(Vocabulary::default().is_empty());
        assert!(vocab(&["", "  "]).is_empty());
        assert_eq!(Vocabulary::default().apply("Hello."), "Hello.");
        assert!(!vocab(&["a"]).is_empty());
    }

    #[test]
    fn text_with_multibyte_characters_keeps_its_bytes() {
        let v = vocab(&["naive => Naïve"]);
        assert_eq!(v.apply("é naive é"), "é Naïve é");
        assert_eq!(v.apply("éé"), "éé");
    }

    #[test]
    #[ignore = "timing, run with --release"]
    fn a_dictation_is_corrected_within_the_budget() {
        let mut entries: Vec<String> = (0..98).map(|i| format!("term{i}x:Term{i}X")).collect();
        entries.extend(["my sequel => MySQL".to_owned(), "Kubernetes".to_owned()]);
        let v = Vocabulary::new(&entries);
        let text = "we talked about my sequel and the kubernettes cluster for a while \
                    and then went home to eat dinner with the whole family tonight";
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            assert!(v.apply(std::hint::black_box(text)).contains("MySQL"));
        }
        let us = started.elapsed().as_micros() / 1000;
        eprintln!("vocabulary apply: {us} us per call");
        assert!(us < 50, "{us} us");
    }
}
