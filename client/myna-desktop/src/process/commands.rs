//! Spoken editing commands, applied to the whole dictation before any of it
//! is typed: "scratch that" takes back the sentence before it, "new line"
//! and "new paragraph" break the text, "delete last word" and "delete last
//! three words" take words back, and "format as a list" (or "numbered
//! list") makes the sentences since the last break a list, one a line.
//!
//! A command counts only as a sentence of its own ("Monday. Scratch that.
//! Tuesday."), which is how a speech model writes a phrase said between
//! pauses; inside a sentence ("scratch that idea") it is typed as said.

/// What one sentence of the dictation is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Block {
    Text(String),
    Break(&'static str),
    /// Items, numbered or bulleted, each on a line of its own.
    List(Vec<String>, bool),
    /// "Format the following as a list.": the sentences after it, up to
    /// the next break, are its items.
    ListStart(bool),
}

/// What a sentence says, if it is a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    /// Take back the sentence before.
    Scratch,
    /// Take back this many words.
    DeleteWords(usize),
    Break(&'static str),
    /// Make the sentences since the last break a list, numbered or not.
    List(bool),
}

/// The sentences of `text`, each with what ends it.
pub(super) fn sentences(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if matches!(c, '.' | '!' | '?' | '\u{2026}') {
            // Closing marks stay with their sentence: "Stop!?" is one.
            while let Some(&(_, next)) = chars.peek() {
                if matches!(next, '.' | '!' | '?' | '\u{2026}' | '"' | '\u{201d}' | ')') {
                    chars.next();
                } else {
                    break;
                }
            }
            let end = chars.peek().map_or(text.len(), |&(j, _)| j);
            if chars.peek().is_none_or(|&(_, next)| next.is_whitespace()) {
                out.push(text[start..end].trim());
                start = end;
            }
        }
    }
    out.push(text[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// The words of `sentence`, lowercased, without punctuation.
fn words(sentence: &str) -> Vec<String> {
    sentence
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

fn number(word: &str) -> Option<usize> {
    const NAMES: [&str; 10] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    NAMES
        .iter()
        .position(|n| *n == word)
        .map(|i| i + 1)
        .or_else(|| word.parse().ok().filter(|n| (1..=50).contains(n)))
}

fn command(sentence: &str) -> Option<Command> {
    let words = words(sentence);
    let said: Vec<&str> = words.iter().map(String::as_str).collect();
    Some(match said.as_slice() {
        ["scratch" | "strike" | "delete" | "undo", "that"] => Command::Scratch,
        ["new", "line"] => Command::Break("\n"),
        ["new", "paragraph"] => Command::Break("\n\n"),
        ["format", "as", "a", "list"] | ["make", "that", "a", "list"] | ["bullet", "list"] => {
            Command::List(false)
        }
        ["format", "as", "a", "numbered", "list"]
        | ["make", "that", "a", "numbered", "list"]
        | ["numbered", "list"] => Command::List(true),
        ["delete", "last", "word"] | ["delete", "the", "last", "word"] => Command::DeleteWords(1),
        ["delete", "last", n, "words"] | ["delete", "the", "last", n, "words"] => {
            Command::DeleteWords(number(n)?)
        }
        _ => return None,
    })
}

/// A command that leads `sentence` before a comma, and the words after:
/// "Scratch that, Tuesday." Without the comma ("Delete that file.") the
/// sentence is text.
fn leading(sentence: &str) -> Option<(Command, &str)> {
    let (head, rest) = sentence.split_once(',')?;
    let rest = rest.trim();
    let cmd = command(head)?;
    (!words(rest).is_empty()).then_some((cmd, rest))
}

/// Type the words that followed a command in its sentence. After taken
/// back words they carry on the sentence they were taken from ("My name
/// is Peter. Delete the last word, Charles." is "My name is Charles.");
/// otherwise they start a sentence.
fn carry_on(blocks: &mut Vec<Block>, cmd: Command, rest: &str) {
    if let (Command::DeleteWords(_), Some(Block::Text(last))) = (cmd, blocks.last_mut()) {
        let open = last.trim_end_matches(|c: char| !c.is_alphanumeric());
        *last = format!("{open} {rest}");
        return;
    }
    let mut chars = rest.chars();
    let capital: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    blocks.push(Block::Text(capital));
}

/// `text` without its last `n` words, keeping how it ended (". ", "?").
fn drop_words(text: &str, n: usize) -> String {
    let end = text.trim_end();
    let mark_at = end
        .rfind(|c: char| c.is_alphanumeric())
        .map_or(0, |i| i + end[i..].chars().next().map_or(1, char::len_utf8));
    let (body, mark) = end.split_at(mark_at);
    let mut kept: Vec<&str> = body.split_whitespace().collect();
    kept.truncate(kept.len().saturating_sub(n));
    if kept.is_empty() {
        return String::new();
    }
    let body = kept.join(" ");
    let body = body.trim_end_matches(',');
    format!("{body}{mark}")
}

/// `text` with every command it holds carried out.
pub fn apply(text: &str) -> String {
    let quoted = quotes(text);
    let mut acted = quoted != text;
    let text = quoted.as_str();
    let mut blocks: Vec<Block> = Vec::new();
    for sentence in sentences(text) {
        if command(sentence).is_none() {
            if let Some((numbered, rest)) = list_prefix(sentence) {
                acted = true;
                if rest.is_empty() {
                    blocks.push(Block::ListStart(numbered));
                } else {
                    blocks.push(Block::List(split_items(rest), numbered));
                }
                continue;
            }
        }
        // A command, or one that leads its sentence before a comma, the
        // words after it carrying on: "Delete the last word, Charles."
        let (found, rest) = match command(sentence) {
            Some(cmd) => (Some(cmd), None),
            None => match leading(sentence) {
                Some((cmd, rest)) => (Some(cmd), Some(rest)),
                None => (None, None),
            },
        };
        match found {
            None => blocks.push(Block::Text(sentence.to_owned())),
            Some(cmd) => {
                acted = true;
                match cmd {
                    Command::Break(brk) => blocks.push(Block::Break(brk)),
                    Command::Scratch => {
                        if let Some(at) = blocks.iter().rposition(|b| !matches!(b, Block::Break(_)))
                        {
                            blocks.truncate(at);
                        }
                    }
                    Command::List(numbered) => {
                        let from = blocks
                            .iter()
                            .rposition(|b| !matches!(b, Block::Text(_)))
                            .map_or(0, |at| at + 1);
                        let mut items: Vec<String> = blocks
                            .drain(from..)
                            .filter_map(|b| match b {
                                Block::Text(t) => Some(item(&t)),
                                _ => None,
                            })
                            .filter(|t| !t.is_empty())
                            .collect();
                        // One sentence of several things: "Milk, eggs and bread."
                        if let [one] = items.as_slice() {
                            items = split_items(one);
                        }
                        if !items.is_empty() {
                            blocks.push(Block::List(items, numbered));
                        }
                    }
                    Command::DeleteWords(mut n) => {
                        while n > 0 {
                            let Some(at) = blocks.iter().rposition(|b| matches!(b, Block::Text(_)))
                            else {
                                break;
                            };
                            let Block::Text(last) = &blocks[at] else {
                                unreachable!()
                            };
                            let count = last.split_whitespace().count();
                            let kept = drop_words(last, n);
                            n = n.saturating_sub(count);
                            if kept.is_empty() {
                                blocks.truncate(at);
                            } else {
                                blocks[at] = Block::Text(kept);
                                blocks.truncate(at + 1);
                                n = 0;
                            }
                        }
                    }
                }
                if let Some(rest) = rest {
                    carry_on(&mut blocks, cmd, rest);
                }
            }
        }
    }
    if !acted {
        return text.to_owned();
    }
    let blocks = open_lists(blocks);
    let mut out = String::new();
    // What the next text needs before it: nothing, a space, or a line.
    let mut gap = "";
    for block in blocks {
        match block {
            Block::Break(brk) => {
                out.push_str(brk);
                gap = "";
            }
            Block::Text(t) => {
                out.push_str(gap);
                out.push_str(&t);
                gap = " ";
            }
            Block::ListStart(_) => {}
            Block::List(items, numbered) => {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                let lines: Vec<String> = items
                    .iter()
                    .enumerate()
                    .map(|(i, t)| match numbered {
                        true => format!("{}. {t}", i + 1),
                        false => format!("- {t}"),
                    })
                    .collect();
                out.push_str(&lines.join("\n"));
                gap = "\n";
            }
        }
    }
    out
}

/// `text` with each spoken quotation ("quote, ... end quote") between
/// quotation marks, the marks speech put around the words it said taken
/// away: `lane, "for your exit".` A "quote" with no end is typed as said.
fn quotes(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let bare = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let opens = |i: usize| {
        bare(words[i]) == "quote"
            && (i == 0 || !matches!(bare(words[i - 1]).as_str(), "end" | "close" | "unquote"))
    };
    // The index of the closing word and how many words it takes.
    let closes = |i: usize| -> Option<usize> {
        match bare(words[i]).as_str() {
            "unquote" => Some(1),
            "end" | "close" if words.get(i + 1).is_some_and(|w| bare(w) == "quote") => Some(2),
            _ => None,
        }
    };
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut at = 0;
    while at < words.len() {
        let close = opens(at)
            .then(|| (at + 1..words.len()).find_map(|j| closes(j).map(|n| (j, n))))
            .flatten();
        let Some((end, len)) = close.filter(|(end, _)| *end > at + 1) else {
            out.push(words[at].to_owned());
            at += 1;
            continue;
        };
        let inner = words[at + 1..end].join(" ");
        let inner = inner.trim_matches(|c: char| c == ',' || c.is_whitespace());
        let last = words[end + len - 1];
        let after = &last[last.rfind(char::is_alphanumeric).map_or(0, |i| i + 1)..];
        out.push(format!("\"{inner}\"{after}"));
        at = end + len;
    }
    if out.len() == words.len() {
        return text.to_owned();
    }
    out.join(" ")
}

/// The blocks with each "format the following as a list" made a list of
/// the sentences after it, up to the next break.
fn open_lists(blocks: Vec<Block>) -> Vec<Block> {
    let mut out = Vec::with_capacity(blocks.len());
    let mut open: Option<(bool, Vec<String>)> = None;
    for block in blocks {
        match (block, &mut open) {
            (Block::Text(t), Some((_, items))) => items.push(item(&t)),
            (Block::ListStart(numbered), _) => {
                if let Some((n, items)) = open.take() {
                    out.push(Block::List(items, n));
                }
                open = Some((numbered, Vec::new()));
            }
            (other, _) => {
                if let Some((n, items)) = open.take() {
                    out.push(Block::List(items, n));
                }
                out.push(other);
            }
        }
    }
    if let Some((n, items)) = open {
        out.push(Block::List(items, n));
    }
    out.retain(|b| !matches!(b, Block::List(items, _) if items.is_empty()));
    out
}

/// "format [the following | this | these] as a [numbered | bulleted] list"
/// at the start of a sentence: whether numbered, and what follows it.
fn list_prefix(sentence: &str) -> Option<(bool, &str)> {
    let mut rest = sentence.trim_start();
    let mut take = |options: &[&str]| -> Option<String> {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = rest[..end]
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        options.contains(&word.as_str()).then(|| {
            rest = rest[end..].trim_start();
            word
        })
    };
    take(&["format"])?;
    if take(&["the"]).is_some() {
        take(&["following"])?;
    } else {
        take(&["this", "these"]);
    }
    take(&["as"])?;
    take(&["a"])?;
    let numbered = take(&["numbered", "bulleted"]).is_some_and(|w| w == "numbered");
    take(&["list"])?;
    Some((numbered, rest.trim_start_matches([':', ',', ' '])))
}

/// The things a sentence lists, by its commas: "an apple, a pear, and a
/// box" is three; with no comma it is one.
fn split_items(text: &str) -> Vec<String> {
    item(text)
        .split(',')
        .map(|part| {
            let part = part.trim();
            part.strip_prefix("and ")
                .or_else(|| part.strip_prefix("or "))
                .unwrap_or(part)
                .trim()
                .to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect()
}

/// A sentence as a list item: without the full stop that ended it.
fn item(sentence: &str) -> String {
    let sentence = sentence.trim();
    match sentence.strip_suffix('.') {
        Some(bare) if !bare.ends_with('.') => bare.trim_end().to_owned(),
        _ => sentence.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scratch_that_takes_back_the_sentence_before_it() {
        assert_eq!(
            apply("Meet on Monday. Scratch that. Meet on Tuesday."),
            "Meet on Tuesday."
        );
        assert_eq!(apply("One. Two. Delete that! Three."), "One. Three.");
        assert_eq!(apply("Scratch that."), "");
        assert_eq!(apply("Hello. Undo that"), "");
    }

    #[test]
    fn a_command_may_lead_its_sentence_before_a_comma() {
        for (said, typed) in [
            (
                "My name is Peter. Delete the last word, Charles.",
                "My name is Charles.",
            ),
            (
                "My name is Peter. Delete last word, Charles.",
                "My name is Charles.",
            ),
            ("Meet Monday. Scratch that, meet Tuesday.", "Meet Tuesday."),
            (
                "Dear Ann. New paragraph, thanks for calling.",
                "Dear Ann.\n\nThanks for calling.",
            ),
            (
                "Send it to the whole team. Delete last two words, group.",
                "Send it to the group.",
            ),
            ("Delete the last word, Charles.", "Charles."),
        ] {
            assert_eq!(apply(said), typed, "{said}");
        }
        for said in ["Delete that file, please.", "Scratch that idea, it is bad."] {
            assert_eq!(apply(said), said);
        }
    }

    #[test]
    fn a_command_inside_a_sentence_is_typed_as_said() {
        for said in [
            "Let's scratch that idea.",
            "I said new line, not newline.",
            "Please delete that file. Thanks.",
            "Delete the last word of the title.",
        ] {
            assert_eq!(apply(said), said);
        }
    }

    #[test]
    fn new_line_and_new_paragraph_break_the_text() {
        assert_eq!(
            apply("Dear Ann. New paragraph. Thanks for the call. New line. Bob."),
            "Dear Ann.\n\nThanks for the call.\nBob."
        );
        assert_eq!(apply("Hi. New line"), "Hi.\n");
    }

    #[test]
    fn delete_last_words_takes_back_words_and_keeps_the_mark() {
        assert_eq!(
            apply("Send it to the whole team. Delete last two words."),
            "Send it to the."
        );
        assert_eq!(apply("Is it ready? Delete the last word."), "Is it?");
        assert_eq!(apply("Go now. Stop here. Delete last 3 words."), "Go.");
        assert_eq!(
            apply("Hi there, Delete last word."),
            "Hi there, Delete last word."
        );
    }

    #[test]
    fn format_as_a_list_lists_the_sentences_since_the_last_break() {
        assert_eq!(
            apply("Milk. Eggs. Bread. Format as a list."),
            "- Milk\n- Eggs\n- Bread"
        );
        assert_eq!(
            apply("We need three things. New line. Milk. Eggs? Bread. Numbered list. Thanks."),
            "We need three things.\n1. Milk\n2. Eggs?\n3. Bread\nThanks."
        );
        assert_eq!(apply("Format as a list."), "");
        assert_eq!(
            apply("A. New paragraph. B. C. Format as a list."),
            "A.\n\n- B\n- C",
            "the break before the items stays"
        );
        assert_eq!(
            apply("A. B. Format as a list. Scratch that. C."),
            "C.",
            "scratch takes back the whole list"
        );
        assert_eq!(
            apply("Make that a list of names."),
            "Make that a list of names."
        );
    }

    #[test]
    fn a_list_can_be_asked_for_first_or_be_one_sentence_of_things() {
        assert_eq!(
            apply("Format the following as a list: an apple, a pear, and a box of cranberries."),
            "- an apple\n- a pear\n- a box of cranberries"
        );
        assert_eq!(
            apply("Format this as a numbered list. Milk. Eggs. New line. Done."),
            "1. Milk\n2. Eggs\nDone."
        );
        assert_eq!(
            apply("One cucumber, three pears, six lemons. Make that a list."),
            "- One cucumber\n- three pears\n- six lemons"
        );
        assert_eq!(
            apply("Salt and pepper. Format as a list."),
            "- Salt and pepper"
        );
        assert_eq!(
            apply("Format the following as a list."),
            "",
            "nothing to list"
        );
        assert_eq!(
            apply("Format the text please."),
            "Format the text please.",
            "not a list"
        );
    }

    #[test]
    fn a_spoken_quotation_is_put_in_quotation_marks() {
        assert_eq!(
            apply("Choose the correct lane, quote, for your exit as soon as you can, end quote."),
            "Choose the correct lane, \"for your exit as soon as you can\"."
        );
        assert_eq!(
            apply("He said quote no way unquote and left."),
            "He said \"no way\" and left."
        );
        assert_eq!(apply("Get a quote from them."), "Get a quote from them.");
        assert_eq!(apply("quote end quote"), "quote end quote");
    }

    #[test]
    fn text_with_no_command_is_left_exactly_as_it_was() {
        let said = "  Spaced   out.  Wow!  ";
        assert_eq!(apply(said), said);
    }

    #[test]
    fn sentences_split_after_marks_followed_by_space() {
        assert_eq!(
            sentences("A. B?! C\u{2026} e.g. x 3.5 D"),
            ["A.", "B?!", "C\u{2026}", "e.g.", "x 3.5 D"]
        );
    }
}
