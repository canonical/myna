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
fn sentences(text: &str) -> Vec<&str> {
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
    let mut blocks: Vec<Block> = Vec::new();
    let mut acted = false;
    for sentence in sentences(text) {
        match command(sentence) {
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
                        let items: Vec<String> = blocks
                            .drain(from..)
                            .filter_map(|b| match b {
                                Block::Text(t) => Some(item(&t)),
                                _ => None,
                            })
                            .filter(|t| !t.is_empty())
                            .collect();
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
            }
        }
    }
    if !acted {
        return text.to_owned();
    }
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
