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
        assert!(!is_chinese("hello, 42"));
        assert!(!is_chinese(""));
        assert!(!is_chinese("こんにちは"));
        assert!(!is_chinese("한국어"));
        assert!(!is_chinese("漢字 한"));
    }

    #[test]
    fn a_character_with_two_readings_follows_its_word() {
        assert_eq!(convert("我的头发", Traditional), "我的頭髮");
        assert_eq!(convert("我们出发", Traditional), "我們出發");
    }
}
