//! Chinese script conversion: OpenCC dictionaries with Jieba segmentation,
//! so a character that reads two ways (`发` in `头发` and `出发`) and a word
//! that differs by region (`软件`, Taiwan `軟體`) come out right.

use std::sync::OnceLock;

use myna_core::ChineseScript;
use opencc_jieba_rs::{OpenCC, OpenccConfig};

static OPENCC: OnceLock<OpenCC> = OnceLock::new();

/// The dictionaries load once, on first use or from [`warm`].
fn opencc() -> &'static OpenCC {
    OPENCC.get_or_init(OpenCC::new)
}

/// Load the dictionaries in the background, so the first sentence does not
/// wait for them.
pub fn warm() -> std::thread::JoinHandle<()> {
    std::thread::spawn(|| {
        opencc();
    })
}

/// Whether `script` is to be applied to dictation in `language`.
pub fn applies(script: ChineseScript, language: Option<&str>) -> bool {
    script != ChineseScript::AsRecognised && myna_core::chains::may_be_chinese(language)
}

fn is_han(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2FA1F}')
}

fn is_kana_or_hangul(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30FF}' | '\u{AC00}'..='\u{D7AF}' | '\u{1100}'..='\u{11FF}')
}

/// Whether `text` has Chinese in it: ideographs, and no kana or hangul
/// (which share the ideographs but make it Japanese or Korean). Unlike
/// `may_be_chinese` this reads the text, not the language code.
fn is_chinese(text: &str) -> bool {
    text.chars().any(is_han) && !text.chars().any(is_kana_or_hangul)
}

/// `text` in `script`; text that is not Chinese is returned as it is.
pub fn convert(text: &str, script: ChineseScript) -> String {
    if script == ChineseScript::AsRecognised || !is_chinese(text) {
        return text.to_owned();
    }
    let config = match script {
        ChineseScript::AsRecognised => return text.to_owned(),
        // Taiwan to Simplified also turns Taiwan words (`軟體`) into the
        // mainland ones, and leaves mainland Traditional alone.
        ChineseScript::Simplified => OpenccConfig::Tw2sp,
        ChineseScript::Traditional => OpenccConfig::S2t,
        ChineseScript::TraditionalTaiwan => OpenccConfig::S2twp,
        ChineseScript::TraditionalHongKong => OpenccConfig::S2hk,
    };
    opencc().convert_with_config(text, config, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ChineseScript::*;

    #[test]
    fn only_ideographs_without_kana_or_hangul_are_chinese() {
        assert!(is_chinese("你好 hello"));
        assert!(is_chinese("\u{20000}"));
        assert!(!is_chinese("hello, 42"));
        assert!(!is_chinese(""));
        assert!(!is_chinese("こんにちは"));
        assert!(!is_chinese("한국어"));
        assert!(!is_chinese("日本語です"));
        assert!(!is_chinese("漢字 한"));
    }

    #[test]
    fn warming_loads_the_dictionaries_in_the_background() {
        warm().join().unwrap();
        assert!(OPENCC.get().is_some());
    }

    #[test]
    fn a_character_with_two_readings_follows_its_word() {
        assert_eq!(convert("我的头发", Traditional), "我的頭髮");
        assert_eq!(convert("我们出发", Traditional), "我們出發");
    }

    #[test]
    fn taiwan_uses_its_own_words() {
        assert_eq!(convert("这个软件", TraditionalTaiwan), "這個軟體");
        assert_eq!(convert("这个软件", Traditional), "這個軟件");
        assert_eq!(convert("這個軟件", TraditionalTaiwan), "這個軟體");
    }

    #[test]
    fn hong_kong_keeps_its_characters() {
        assert_eq!(convert("今天的天气", TraditionalHongKong), "今天的天氣");
    }

    #[test]
    fn simplified_converts_traditional_text() {
        assert_eq!(convert("今天的天氣很好", Simplified), "今天的天气很好");
        assert_eq!(convert("今天的天气很好", Simplified), "今天的天气很好");
        assert_eq!(convert("這個軟體", Simplified), "这个软件");
    }

    #[test]
    fn as_recognised_changes_nothing() {
        assert_eq!(convert("头发", AsRecognised), "头发");
        assert_eq!(convert("頭髮", AsRecognised), "頭髮");
    }

    #[test]
    fn latin_text_and_marks_are_untouched() {
        for script in [
            Simplified,
            Traditional,
            TraditionalTaiwan,
            TraditionalHongKong,
        ] {
            assert_eq!(
                convert("Hello, World! NASA 42", script),
                "Hello, World! NASA 42"
            );
        }
        assert_eq!(
            convert("我用 GitHub 看 README.md, v2.0 好", Traditional),
            "我用 GitHub 看 README.md, v2.0 好"
        );
    }

    #[test]
    fn japanese_and_korean_are_left_alone() {
        assert_eq!(convert("国の言葉です", Traditional), "国の言葉です");
        assert_eq!(convert("한국 国", Traditional), "한국 国");
    }

    #[test]
    fn it_applies_to_chinese_or_an_unknown_language_only() {
        assert!(!applies(AsRecognised, None));
        assert!(applies(Traditional, None));
        for l in ["zh", "zh-TW", "zh_CN", "yue", "ZH"] {
            assert!(applies(Simplified, Some(l)), "{l}");
        }
        for l in ["en", "ja", "zhx", "fr"] {
            assert!(!applies(Simplified, Some(l)), "{l}");
        }
    }

    #[test]
    fn a_piece_of_a_sentence_converts_in_microseconds() {
        convert("热身", Traditional);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            convert("我今天去公司开会，然后回家吃饭。", TraditionalTaiwan);
        }
        assert!(start.elapsed() / 100 < std::time::Duration::from_millis(50));
    }
}
