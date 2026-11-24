//! Spoken numbers as digits: "twenty five" is typed as 25, "three point
//! five" as 3.5, "fifty percent" as 50%, "ten dollars" as $10. Numbers
//! under ten stay words in prose ("two kids"), as style guides write them,
//! unless a unit or a currency follows.

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// What follows a number and how it is written with it.
enum Unit {
    /// A symbol before the number: "$10".
    Before(&'static str),
    /// A symbol after it: "50%".
    After(&'static str),
    /// A word kept after it: "10 km" stays "10 kilometres".
    Word,
}

fn unit(word: &str) -> Option<Unit> {
    Some(match word {
        "dollars" | "dollar" | "bucks" => Unit::Before("$"),
        "euros" | "euro" => Unit::Before("\u{20ac}"),
        "pounds" | "pound" | "quid" => Unit::Before("\u{a3}"),
        "percent" => Unit::After("%"),
        "kilometres" | "kilometers" | "km" | "metres" | "meters" | "miles" | "kilos"
        | "kilograms" | "grams" | "litres" | "liters" | "minutes" | "seconds" | "hours"
        | "days" | "weeks" | "months" | "years" | "gigabytes" | "megabytes" | "degrees" => {
            Unit::Word
        }
        _ => return None,
    })
}

/// A word as said: lowercased letters, and the marks around it.
fn bare(raw: &str) -> (String, &str, &str) {
    let start = raw.find(char::is_alphanumeric).unwrap_or(raw.len());
    let end = raw.rfind(char::is_alphanumeric).map_or(start, |i| {
        i + raw[i..].chars().next().map_or(1, char::len_utf8)
    });
    (raw[start..end].to_lowercase(), &raw[..start], &raw[end..])
}

/// The value of a number word below a hundred.
fn small(word: &str) -> Option<u64> {
    if let Some(n) = ONES.iter().position(|w| *w == word) {
        return Some(n as u64);
    }
    if let Some(t) = TENS.iter().position(|w| *w == word) {
        return Some(20 + 10 * t as u64);
    }
    // "twenty-five"
    let (tens, ones) = word.split_once('-')?;
    let t = TENS.iter().position(|w| *w == tens)?;
    let o = ONES[1..10].iter().position(|w| *w == ones)?;
    Some(20 + 10 * t as u64 + 1 + o as u64)
}

fn scale(word: &str) -> Option<u64> {
    Some(match word {
        "hundred" => 100,
        "thousand" => 1_000,
        "million" => 1_000_000,
        "billion" => 1_000_000_000,
        _ => return None,
    })
}

/// The number spoken from word `start`: its value, its decimals, the index
/// after it, and whether it was more than one word.
fn number(words: &[(String, &str, &str)], start: usize) -> Option<(u64, String, usize)> {
    let mut total = 0u64;
    let mut current = 0u64;
    let mut at = start;
    let mut any = false;
    // Only the last word of the number may carry a mark.
    while at < words.len() {
        let (w, lead, _) = &words[at];
        if at > start && !lead.is_empty() {
            break;
        }
        if let Some(n) = small(w) {
            // "twenty five" and "a hundred and five" are one number each;
            // "five six" and "twelve thirteen" are two.
            let tail = current % 100;
            let joins = !any || tail == 0 || (tail >= 20 && tail % 10 == 0 && n < 10);
            if !joins {
                break;
            }
            current += n;
        } else if let Some(s) = scale(w) {
            if !any && w != "hundred" {
                break;
            }
            let base = if current == 0 { 1 } else { current };
            if s == 100 {
                current = base * 100;
            } else {
                total += base * s;
                current = 0;
            }
        } else if w == "and"
            && any
            && at + 1 < words.len()
            && small(&words[at + 1].0).is_some()
            && (total > 0 || current >= 100)
        {
            // "a hundred and five"
        } else if w == "a" && !any && words.get(at + 1).is_some_and(|n| scale(&n.0).is_some()) {
            // "a hundred": the scale that follows counts it once.
            current = 1;
        } else {
            break;
        }
        any = true;
        at += 1;
        if !words[at - 1].2.is_empty() {
            break;
        }
    }
    if !any {
        return None;
    }
    // "three point one four"
    let mut decimals = String::new();
    if words[at - 1].2.is_empty()
        && words
            .get(at)
            .is_some_and(|w| w.0 == "point" && w.1.is_empty())
    {
        let mut d = at + 1;
        while let Some((w, lead, tail)) = words.get(d) {
            let Some(n) = small(w).filter(|n| *n < 10) else {
                break;
            };
            if !lead.is_empty() {
                break;
            }
            decimals.push(char::from(b'0' + n as u8));
            d += 1;
            if !tail.is_empty() {
                break;
            }
        }
        if !decimals.is_empty() {
            at = d;
        }
    }
    Some((total + current, decimals, at))
}

pub fn apply(text: &str) -> String {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<(String, &str, &str)> = raw.iter().map(|r| bare(r)).collect();
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < words.len() {
        let Some((value, decimals, end)) = number(&words, at) else {
            out.push(raw[at].to_owned());
            at += 1;
            continue;
        };
        let last_tail = words[end - 1].2;
        let next = words
            .get(end)
            .filter(|_| last_tail.is_empty())
            .map(|(w, lead, _)| (w.as_str(), lead.is_empty()));
        let (unit_word, percent_two) = match next {
            Some(("per", true)) if words.get(end + 1).is_some_and(|w| w.0 == "cent") => {
                (Some(Unit::After("%")), true)
            }
            Some((w, true)) => (unit(w), false),
            _ => (None, false),
        };
        let digits = if decimals.is_empty() {
            value.to_string()
        } else {
            format!("{value}.{decimals}")
        };
        let worth = value >= 10 || !decimals.is_empty() || unit_word.is_some();
        // "one" alone is a word as often as a number: "one of them".
        if !worth || (value == 1 && end - at == 1 && unit_word.is_none()) {
            out.extend(raw[at..end].iter().map(|w| (*w).to_owned()));
            at = end;
            continue;
        }
        let lead = words[at].1;
        match unit_word {
            Some(Unit::Before(symbol)) => {
                let (_, _, tail) = &words[end];
                out.push(format!("{lead}{symbol}{digits}{tail}"));
                at = end + 1;
            }
            Some(Unit::After(symbol)) => {
                let used = if percent_two { 2 } else { 1 };
                let (_, _, tail) = &words[end + used - 1];
                out.push(format!("{lead}{digits}{symbol}{tail}"));
                at = end + used;
            }
            Some(Unit::Word) | None => {
                out.push(format!("{lead}{digits}{last_tail}"));
                at = end;
            }
        }
    }
    let typed = out.join(" ");
    if typed.split_whitespace().eq(text.split_whitespace()) {
        return text.to_owned();
    }
    typed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_of_ten_and_over_are_digits() {
        for (said, typed) in [
            ("twenty five people came", "25 people came"),
            ("twenty-five people", "25 people"),
            ("one hundred and twenty three", "123"),
            ("a hundred and five", "105"),
            ("two thousand twenty six", "2026"),
            ("three million", "3000000"),
            ("It took eleven days.", "It took 11 days."),
            ("Twelve.", "12."),
        ] {
            assert_eq!(apply(said), typed, "{said}");
        }
    }

    #[test]
    fn small_numbers_in_prose_stay_words() {
        for said in [
            "I have two kids.",
            "one of them",
            "Five of us went.",
            "a hundredfold",
            "seven",
        ] {
            assert_eq!(apply(said), said);
        }
    }

    #[test]
    fn units_decimals_and_money_make_any_number_digits() {
        for (said, typed) in [
            ("three point five", "3.5"),
            ("pi is three point one four.", "pi is 3.14."),
            ("fifty percent", "50%"),
            ("five per cent off", "5% off"),
            ("ten dollars", "$10"),
            ("It costs twenty five euros.", "It costs \u{20ac}25."),
            ("two pounds, please", "\u{a3}2, please"),
            ("five kilometres", "5 kilometres"),
        ] {
            assert_eq!(apply(said), typed, "{said}");
        }
    }

    #[test]
    fn separate_numbers_stay_separate() {
        assert_eq!(apply("twelve thirteen"), "12 13");
        assert_eq!(apply("five six seven eight ten"), "five six seven eight 10");
    }
}
