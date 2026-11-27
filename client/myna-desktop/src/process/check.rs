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

    /// The cases `server/tests/benchmarker/test_text_metrics.py` also runs, so
    /// the benchmarker's reference check cannot drift from this one. Set
    /// `MYNA_REGEN_CASES=1` to rewrite the file from the vendored corpus.
    ///
    /// One case per verdict path, which is what the file is deduplicated to:
    /// two cases are the same path when the check reaches the same verdict
    /// with the same number of heard and answer words and the same kind of
    /// written entry (none, a plain term, or a `spoken => written` one). A
    /// longer run of the same shape exercises no branch its representative
    /// does not, and the verdict is the branch it ended in.
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

    /// The written entry as the check sees it: plain terms and
    /// `spoken => written` entries are two different paths.
    fn written_kind(written: &[String]) -> &'static str {
        if written.is_empty() {
            "none"
        } else if written.iter().any(|w| w.contains("=>")) {
            "spoken"
        } else {
            "plain"
        }
    }

    /// What makes two cases the same verdict path.
    fn verdict_path(case: &serde_json::Value) -> (String, usize, usize, &'static str) {
        let words = |field: &str| case[field].as_str().unwrap().split_whitespace().count();
        let written: Vec<String> = case["written"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w.as_str().unwrap().to_owned())
            .collect();
        (
            case["verdict"].as_str().unwrap().to_owned(),
            words("heard"),
            words("answer"),
            written_kind(&written),
        )
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
            ("gee nome here", "Gnome here", &["Gnome"]),
            ("", "hello", &[]),
            ("今天天气很好啊啊啊啊", "今天天气很好", &[]),
            ("今日は、えーと、晴れです", "今日は晴れです", &[]),
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
        let mut seen = std::collections::HashSet::new();
        cases
            .into_iter()
            .map(|(heard, answer, written)| {
                let verdict =
                    verdict_name(Check::ConservativeEdit.verdict(&heard, &answer, &written));
                serde_json::json!({
                    "heard": heard, "answer": answer, "written": written, "verdict": verdict,
                })
            })
            .filter(|case| seen.insert(verdict_path(case)))
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
        assert!(
            cases.len() > 60,
            "one case per verdict path is there, {} found",
            cases.len()
        );
        let mut paths = std::collections::HashSet::new();
        for case in &cases {
            assert!(
                paths.insert(verdict_path(case)),
                "{} is not a new path",
                case
            );
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
}
