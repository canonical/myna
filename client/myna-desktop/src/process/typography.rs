//! Typography as the text's language writes it: curly quotes and
//! apostrophes, a spaced dash for " - ", an ellipsis for "...", and in
//! French the narrow no-break spaces before ! ? ; and a no-break space
//! before : and inside « ».

/// Which conventions apply; anything not named gets the ones every
/// language shares (apostrophes, dashes, the ellipsis) and keeps its
/// double quotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    English,
    French,
    German,
    Other,
}

impl Style {
    pub fn of(language: Option<&str>) -> Self {
        match language {
            Some("en") => Style::English,
            Some("fr") => Style::French,
            Some("de") => Style::German,
            _ => Style::Other,
        }
    }

    /// The opening and closing double quotes, if the style has them.
    fn quotes(self) -> Option<(&'static str, &'static str)> {
        match self {
            Style::English => Some(("\u{201c}", "\u{201d}")),
            Style::French => Some(("\u{ab}\u{a0}", "\u{a0}\u{bb}")),
            Style::German => Some(("\u{201e}", "\u{201c}")),
            Style::Other => None,
        }
    }
}

const NNBSP: char = '\u{202f}';
const NBSP: char = '\u{a0}';

/// Whether a quote after `before` opens rather than closes.
fn opens(before: Option<char>) -> bool {
    before.is_none_or(|c| c.is_whitespace() || "([{\u{2014}\u{2013}-".contains(c))
}

/// Whether a single quote further on closes one: "'really'" quotes,
/// "'em" and "'90s" only leave letters out.
fn closes_later(rest: &[char]) -> bool {
    rest.iter()
        .enumerate()
        .any(|(i, c)| *c == '\'' && rest.get(i + 1).is_none_or(|n| !n.is_alphanumeric()))
}

pub fn apply(text: &str, style: Style) -> String {
    let text = text.replace("...", "\u{2026}").replace(" - ", " \u{2013} ");
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len() + 8);
    for (i, &c) in chars.iter().enumerate() {
        let before = out.chars().next_back();
        let after = chars.get(i + 1).copied();
        match c {
            '\'' => {
                let letter = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
                if letter(before) || !opens(before) {
                    out.push('\u{2019}');
                } else if after.is_some_and(char::is_alphabetic)
                    && style == Style::English
                    && closes_later(&chars[i + 1..])
                {
                    out.push('\u{2018}');
                } else {
                    out.push('\u{2019}');
                }
            }
            '"' => match style.quotes() {
                Some((open, close)) => {
                    if opens(before) {
                        out.push_str(open);
                    } else {
                        if style == Style::French && out.ends_with(' ') {
                            out.pop();
                        }
                        out.push_str(close);
                    }
                }
                None => out.push(c),
            },
            '!' | '?' | ';' | ':' if style == Style::French => {
                // "Bonjour!" and "Bonjour !" both become "Bonjour\u{202f}!";
                // a colon takes the wider no-break space. Not inside a
                // word or number ("12:30", "http://").
                let space = if c == ':' { NBSP } else { NNBSP };
                let word_before = out.trim_end_matches(' ').chars().next_back();
                let inside = c == ':' && after.is_some_and(|a| !a.is_whitespace());
                if word_before.is_some_and(|w| w.is_alphanumeric() || "\u{bb})".contains(w))
                    && !inside
                {
                    while out.ends_with(' ') {
                        out.pop();
                    }
                    if !out.ends_with([NNBSP, NBSP]) {
                        out.push(space);
                    }
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    if style == Style::French {
        // The space typed after an opening « is the no-break one already.
        out = out.replace("\u{ab}\u{a0} ", "\u{ab}\u{a0}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_gets_curly_quotes_and_apostrophes() {
        assert_eq!(
            apply("She said \"it's done\" - 'really'...", Style::English),
            "She said \u{201c}it\u{2019}s done\u{201d} \u{2013} \u{2018}really\u{2019}\u{2026}"
        );
        assert_eq!(apply("the '90s", Style::English), "the \u{2019}90s");
        assert_eq!(
            apply("give 'em hell", Style::English),
            "give \u{2019}em hell"
        );
    }

    #[test]
    fn german_opens_low() {
        assert_eq!(
            apply("Er sagte \"ja\".", Style::German),
            "Er sagte \u{201e}ja\u{201c}."
        );
    }

    #[test]
    fn french_spaces_its_marks_with_no_break_spaces() {
        assert_eq!(
            apply("Bonjour ! Ça va? Voici : \"oui\".", Style::French),
            "Bonjour\u{202f}! Ça va\u{202f}? Voici\u{a0}: \u{ab}\u{a0}oui\u{a0}\u{bb}."
        );
        assert_eq!(
            apply("à 12:30, http://x", Style::French),
            "à 12:30, http://x"
        );
        assert_eq!(apply("l'été", Style::French), "l\u{2019}été");
    }

    #[test]
    fn a_language_with_no_style_keeps_its_double_quotes() {
        assert_eq!(
            apply("Dice \"sì\" - l'ho...", Style::Other),
            "Dice \"sì\" \u{2013} l\u{2019}ho\u{2026}"
        );
    }

    #[test]
    fn typography_is_idempotent() {
        for style in [Style::English, Style::French, Style::German, Style::Other] {
            let once = apply("He said \"it's\" - fine! Ok: \"yes\"...", style);
            assert_eq!(apply(&once, style), once, "{style:?}");
        }
    }
}
