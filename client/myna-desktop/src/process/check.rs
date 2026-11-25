//! What a model's answer must satisfy to be typed in place of what was
//! heard. A failed check names a [`Skip`] code and nothing else.

use std::collections::HashSet;

use myna_core::history::Skip;

/// The longest run of words a cleanup may drop at once.
const LONGEST_DROP: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// A cleanup, whatever the model: the answer keeps what was said in
    /// order and only drops, recases and repunctuates, within bounds. The
    /// user's own words may stand in for what the speech model misheard.
    ConservativeEdit,
    /// A translation: not empty and of a plausible length.
    Usable,
}

impl Check {
    /// Whether `answer` may replace `heard`; `written` is the user's own
    /// vocabulary.
    pub fn verdict(self, heard: &str, answer: &str, written: &[String]) -> Result<(), Skip> {
        if answer.trim().is_empty() {
            return Err(Skip::Empty);
        }
        match self {
            Check::ConservativeEdit => conservative_edit(heard, answer, written),
            Check::Usable => usable(heard, answer),
        }
    }
}

/// Ideographs and kana, which are written without spaces: each one is a
/// word of its own, or a cleanup of Chinese would be one token that either
/// matches whole or not at all.
fn is_unspaced(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'
        | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
}

/// A word of `text`, lowercased and stripped of punctuation; an apostrophe
/// inside a word stays, so "shouldn't" can be told from "should".
fn words(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut flush = |word: &mut String| {
        let kept = word.trim_matches('\'');
        if !kept.is_empty() {
            out.push(kept.to_owned());
        }
        word.clear();
    };
    for raw in text.split_whitespace() {
        for c in raw.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }) {
            if is_unspaced(c) {
                flush(&mut word);
                word.push(c);
                flush(&mut word);
            } else if c.is_alphanumeric() || c == '\'' {
                word.extend(c.to_lowercase());
            }
        }
        flush(&mut word);
    }
    out
}

fn is_negation(word: &str) -> bool {
    matches!(word, "not" | "never" | "cannot") || word.ends_with("n't")
}

/// Whether dropping `run`, the words between two kept ones, is allowed.
fn droppable(run: &[String]) -> Result<(), Skip> {
    match run {
        [word] if is_negation(word) => Err(Skip::DroppedNegation),
        _ if run.len() > LONGEST_DROP => Err(Skip::LongDrop),
        _ => Ok(()),
    }
}

/// The words of the user's vocabulary a misheard run may become.
fn written_words(written: &[String]) -> HashSet<String> {
    written
        .iter()
        .flat_map(|entry| words(super::vocabulary::written(entry)))
        .collect()
}

/// The longest run of heard words one written word may stand in for:
/// "my sequel" for "MySQL".
const LONGEST_MISHEARING: usize = 3;

fn conservative_edit(heard: &str, answer: &str, written: &[String]) -> Result<(), Skip> {
    let heard = words(heard);
    let answer = words(answer);
    if answer.is_empty() {
        return Err(Skip::Empty);
    }
    let written = written_words(written);
    let n = heard.len();
    // dropped[i][j]: the fewest heard words dropped to keep answer[..j]
    // with heard[i - 1] the last word used, or i == 0 before any.
    let mut dropped = vec![vec![None::<usize>; answer.len() + 1]; n + 1];
    dropped[0][0] = Some(0);
    let mut worst = Skip::NewWord;
    for j in 0..answer.len() {
        for i in 0..=n {
            let Some(so_far) = dropped[i][j] else {
                continue;
            };
            // Up to one past the longest drop, so a run just too long is
            // named rather than read as a new word.
            for dropping in 0..=LONGEST_DROP + 1 {
                let next = i + dropping;
                if next >= n {
                    break;
                }
                let run = &heard[i..next];
                let same = heard[next] == answer[j];
                let spans = if same {
                    1..=1
                } else if written.contains(&answer[j]) {
                    1..=LONGEST_MISHEARING
                } else {
                    continue;
                };
                for span in spans {
                    let end = next + span;
                    if end > n || !sounds_alike(&heard[next..end], &answer[j]) {
                        continue;
                    }
                    // A word the user's vocabulary stands in for is dropped
                    // as far as a negation or a long run is concerned, and
                    // all of its words but the one kept count as dropped.
                    let swapped = if same { &[][..] } else { &heard[next..end] };
                    if let Err(why) = droppable(run).and_then(|()| droppable(swapped)) {
                        worst = nearer(worst, why);
                        continue;
                    }
                    let total = so_far + run.len() + swapped.len().saturating_sub(1);
                    let cell = &mut dropped[end][j + 1];
                    *cell = Some(cell.map_or(total, |best| best.min(total)));
                }
            }
        }
    }
    let budget = 2.max(n.div_ceil(2));
    let mut best = None;
    for (i, row) in dropped.iter().enumerate() {
        let Some(so_far) = row[answer.len()] else {
            continue;
        };
        if let Err(why) = droppable(&heard[i..]) {
            worst = nearer(worst, why);
            continue;
        }
        let total = so_far + (n - i);
        best = Some(best.map_or(total, |b: usize| b.min(total)));
    }
    match best {
        Some(total) if total <= budget => Ok(()),
        Some(_) => Err(Skip::TooManyDropped),
        None => Err(worst),
    }
}

/// Of two reasons no alignment worked, the one nearer to passing: a
/// dropped negation or a long drop explains more than "new word".
fn nearer(so_far: Skip, other: Skip) -> Skip {
    if so_far == Skip::NewWord {
        other
    } else {
        so_far
    }
}

/// Whether `run` may be what the speech model made of `word`: it starts
/// as the word does. Crude on purpose; it only keeps a written word from
/// standing in for any three words.
fn sounds_alike(run: &[String], word: &str) -> bool {
    run.first()
        .and_then(|first| first.chars().next())
        .is_some_and(|c| word.starts_with(c))
}

fn usable(heard: &str, answer: &str) -> Result<(), Skip> {
    let heard = heard.trim();
    let answer = answer.trim();
    let (said, got) = (heard.chars().count(), answer.chars().count());
    // got / said within 0.25..=4.0, in integers.
    if said == 0 || got * 4 < said || got > said * 4 {
        return Err(Skip::OddLength);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tidy(heard: &str, answer: &str) -> Result<(), Skip> {
        Check::ConservativeEdit.verdict(heard, answer, &[])
    }

    #[test]
    fn a_cleanup_may_drop_recase_and_repunctuate() {
        assert_eq!(
            tidy("Um, let's meet Monday, no, Tuesday.", "Let's meet Tuesday."),
            Ok(())
        );
        assert_eq!(tidy("so I I think", "So I think."), Ok(()));
        assert_eq!(tidy("hello world", "Hello, world!"), Ok(()));
    }

    #[test]
    fn a_repair_may_drop_a_negation_with_the_words_around_it() {
        assert_eq!(tidy("Monday, no, Tuesday", "Tuesday"), Ok(()));
        assert_eq!(tidy("we shouldn't, wait, we should", "we should"), Ok(()));
        assert_eq!(tidy("we should not, never mind", "we should"), Ok(()));
    }

    #[test]
    fn a_lone_negation_is_never_dropped() {
        assert_eq!(tidy("do not merge", "do merge"), Err(Skip::DroppedNegation));
        assert_eq!(
            tidy("I never said that", "I said that"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(
            tidy("it isn't ready", "it ready"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(
            tidy("it isn\u{2019}t ready", "it ready"),
            Err(Skip::DroppedNegation)
        );
        assert_eq!(tidy("you cannot go", "you go"), Err(Skip::DroppedNegation));
        assert_eq!(tidy("ship it not", "ship it"), Err(Skip::DroppedNegation));
        assert_eq!(tidy("not now", "now"), Err(Skip::DroppedNegation));
    }

    #[test]
    fn chinese_is_checked_a_character_at_a_time() {
        assert_eq!(tidy("嗯 我们 明天 开会", "我们明天开会。"), Ok(()));
        assert_eq!(tidy("我们明天开会", "开会明天我们"), Err(Skip::NewWord));
        assert_eq!(tidy("我们明天开会", "我们后天开会"), Err(Skip::NewWord));
        assert_eq!(
            tidy("今天天气很好啊啊啊啊啊啊啊啊", "今天天气很好"),
            Err(Skip::LongDrop)
        );
        assert_eq!(tidy("今天天气很好", "今天"), Err(Skip::TooManyDropped));
        // Latin beside it is still a word.
        assert_eq!(tidy("open GitHub 好的", "Open GitHub."), Ok(()));
    }

    #[test]
    fn a_vocabulary_word_may_not_stand_in_for_a_lone_negation() {
        let written = ["nginx".to_owned()];
        let check = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &written);
        assert_eq!(
            check("we do not ship", "we do nginx ship"),
            Err(Skip::DroppedNegation)
        );
        assert!(check("we do never ship", "we do nginx ship").is_err());
        assert_eq!(check("we do ship", "we do nginx"), Err(Skip::NewWord));
        // The same word inside a longer misheard run is a repair.
        assert_eq!(check("run n jinx now", "run nginx now"), Ok(()));
        assert_eq!(
            check("run not now", "run nginx now"),
            Err(Skip::DroppedNegation)
        );
    }

    #[test]
    fn words_a_vocabulary_word_replaces_count_as_dropped() {
        let written = ["MySQL".to_owned()];
        let check = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &written);
        assert_eq!(check("my sequel", "MySQL"), Ok(()));
        // Three heard words become one: two dropped, over the budget of two
        // once a fourth goes.
        assert_eq!(
            check("a b my sequel server", "MySQL"),
            Err(Skip::TooManyDropped)
        );
    }

    #[test]
    fn a_new_moved_or_doubled_word_fails() {
        assert_eq!(
            tidy("meet Tuesday", "Sure! Meet Tuesday."),
            Err(Skip::NewWord)
        );
        assert_eq!(tidy("meet Tuesday", "Tuesday meet"), Err(Skip::NewWord));
        assert_eq!(
            tidy("meet Tuesday", "meet meet Tuesday"),
            Err(Skip::NewWord)
        );
        assert_eq!(tidy("we should", "we shouldn't"), Err(Skip::NewWord));
        assert_eq!(tidy("", "hello"), Err(Skip::NewWord));
    }

    #[test]
    fn a_cleanup_keeps_at_least_half_and_at_least_one_word() {
        assert_eq!(tidy("one", "..."), Err(Skip::Empty));
        assert_eq!(tidy("one two three four five six", "four five six"), Ok(()));
        assert_eq!(
            tidy("one two three four five six", "five six"),
            Err(Skip::TooManyDropped)
        );
        // Two words may always go, however short the text.
        assert_eq!(tidy("um uh right", "Right."), Ok(()));
    }

    #[test]
    fn at_most_six_words_in_a_row_may_go() {
        let heard = "a b c d e f g h i j k l m n";
        assert_eq!(tidy(heard, "a h i j k l m n"), Ok(()));
        assert_eq!(tidy(heard, "a i j k l m n"), Err(Skip::LongDrop));
        assert_eq!(tidy(heard, "h i j k l m n"), Err(Skip::LongDrop));
        assert_eq!(tidy(heard, "a b c d e f g"), Err(Skip::LongDrop));
    }

    #[test]
    fn repeated_words_align_where_the_drops_are_allowed() {
        assert_eq!(tidy("the the the cat", "the cat"), Ok(()));
        // Keeping the first "the" would drop a lone "not"; keeping the
        // second drops it inside a longer run, which a repair may.
        assert_eq!(tidy("x not the a the cat", "x the cat"), Ok(()));
    }

    #[test]
    fn a_written_word_may_stand_in_for_what_was_misheard() {
        let written = ["MySQL".to_owned(), "Visual Studio Code".to_owned()];
        let tidy = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &written);
        assert_eq!(
            tidy("um restart my sequel now", "Restart MySQL now."),
            Ok(())
        );
        assert_eq!(
            tidy("open visual studio code", "Open Visual Studio Code."),
            Ok(())
        );
        assert_eq!(
            tidy("restart now", "Restart MySQL now."),
            Err(Skip::NewWord)
        );
        assert_eq!(
            tidy("restart the my sequel server now", "Restart MySQL now."),
            Ok(())
        );
        assert_eq!(tidy("a b c d", "MySQL"), Err(Skip::NewWord));
        let spoken = ["my sequel => MySQL".to_owned()];
        let tidy = |heard, answer| Check::ConservativeEdit.verdict(heard, answer, &spoken);
        assert_eq!(tidy("restart my sequel", "Restart MySQL."), Ok(()));
        assert_eq!(
            tidy("restart now", "Restart my now."),
            Err(Skip::NewWord),
            "the spoken side of an entry is not a written word"
        );
        // The cheapest reading wins: "MySQL" for "m s q" drops "x" and two
        // more, "MySQL" for "m" alone would drop five, past the budget of 3.
        assert_eq!(tidy("x m s q k", "MySQL k"), Ok(()));
        assert_eq!(
            tidy("restart a sequel now", "restart MySQL now"),
            Err(Skip::NewWord)
        );
    }

    /// The cases `server/tests/benchmarker/test_text_metrics.py` also runs, so
    /// the benchmarker's reference check cannot drift from this one. Set
    /// `MYNA_REGEN_CASES=1` to rewrite the file from the vendored corpus.
    const CASES: &str = "tests/data/conservative_edit_cases.json";

    fn verdict_name(result: Result<(), Skip>) -> String {
        match result {
            Ok(()) => "ok".to_owned(),
            Err(skip) => serde_json::to_value(skip)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned(),
        }
    }

    fn regenerated_cases() -> Vec<serde_json::Value> {
        let hand: &[(&str, &str, &[&str])] = &[
            ("we do not ship", "we do nginx ship", &["nginx"]),
            ("restart my sequel now", "Restart MySQL now.", &["MySQL"]),
            (
                "restart my sequel now",
                "Restart MySQL now.",
                &["my sequel => MySQL"],
            ),
            ("a b my sequel server", "MySQL", &["MySQL"]),
            ("ship it", "ship it Gnome", &["Gnome"]),
            ("gee nome here", "Gnome here", &["Gnome"]),
            ("run not now", "run nginx now", &["nginx"]),
            ("it isn't ready", "it ready", &[]),
            ("", "hello", &[]),
            ("a b c d e", "a b", &[]),
            ("a b c d e f", "a b", &[]),
            ("嗯 我们 明天 开会", "我们明天开会", &[]),
            ("我们不去", "我们去", &[]),
            ("今天天气很好啊啊啊啊", "今天天气很好", &[]),
            ("我们明天开会", "开会明天我们", &[]),
            ("今日は、えーと、晴れです", "今日は晴れです", &[]),
            ("one two", "one two three", &[]),
            ("Monday, no, Tuesday", "Tuesday", &[]),
            ("co-operate fully", "cooperate fully", &[]),
            ("well, it's 3.5 now", "it's 3.5 now", &[]),
        ];
        let corpus = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../server/src/myna/benchmarker/textcorpus/disfluent_en.jsonl"
        ))
        .expect("the vendored corpus");
        let mut cases: Vec<(String, String, Vec<String>)> = hand
            .iter()
            .map(|(h, a, w)| {
                (
                    h.to_string(),
                    a.to_string(),
                    w.iter().map(|w| w.to_string()).collect(),
                )
            })
            .collect();
        for line in corpus.lines() {
            let row: serde_json::Value = serde_json::from_str(line).unwrap();
            let heard = row["heard"].as_str().unwrap().to_owned();
            let intended = row["intended"].as_str().unwrap().to_owned();
            let heard_words: Vec<&str> = heard.split_whitespace().collect();
            let intended_words: Vec<&str> = intended.split_whitespace().collect();
            let mut without_middle = intended_words.clone();
            without_middle.remove(without_middle.len() / 2);
            let mut reversed = intended_words.clone();
            reversed.reverse();
            let every_other: Vec<&str> = heard_words.iter().copied().step_by(2).collect();
            for answer in [
                intended.clone(),
                without_middle.join(" "),
                heard_words[1..].join(" "),
                reversed.join(" "),
                every_other.join(" "),
            ] {
                cases.push((heard.clone(), answer, Vec::new()));
            }
        }
        cases
            .into_iter()
            .map(|(heard, answer, written)| {
                let verdict =
                    verdict_name(Check::ConservativeEdit.verdict(&heard, &answer, &written));
                serde_json::json!({
                    "heard": heard, "answer": answer, "written": written, "verdict": verdict,
                })
            })
            .collect()
    }

    #[test]
    fn the_benchmarkers_reference_check_agrees_on_the_shared_cases() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CASES);
        if std::env::var_os("MYNA_REGEN_CASES").is_some() {
            let mut text = serde_json::to_string_pretty(&regenerated_cases()).unwrap();
            text.push('\n');
            std::fs::write(&path, text).unwrap();
        }
        let cases: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(cases.len() > 500, "the corpus-derived cases are there");
        for case in &cases {
            let written: Vec<String> = case["written"]
                .as_array()
                .unwrap()
                .iter()
                .map(|w| w.as_str().unwrap().to_owned())
                .collect();
            let got = verdict_name(Check::ConservativeEdit.verdict(
                case["heard"].as_str().unwrap(),
                case["answer"].as_str().unwrap(),
                &written,
            ));
            assert_eq!(got, case["verdict"].as_str().unwrap(), "{case}");
        }
    }

    fn usable(heard: &str, answer: &str) -> Result<(), Skip> {
        Check::Usable.verdict(heard, answer, &[])
    }

    #[test]
    fn a_translation_must_be_of_a_plausible_length() {
        assert_eq!(usable("abcd", "a"), Ok(()));
        assert_eq!(usable("abcde", "a"), Err(Skip::OddLength));
        assert_eq!(usable("a", "abcd"), Ok(()));
        assert_eq!(usable("a", "abcde"), Err(Skip::OddLength));
        assert_eq!(usable("", "a"), Err(Skip::OddLength));
        assert_eq!(usable("hello", " "), Err(Skip::Empty));
        assert_eq!(usable("Bien sur.", "Of course."), Ok(()));
    }
}
