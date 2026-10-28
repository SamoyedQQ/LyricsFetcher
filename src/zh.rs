//! 簡繁轉換（MediaWiki 字詞轉換表）。

use zhconv::{Variant, zhconv};

/// 轉為台灣繁體
pub fn to_traditional(text: &str) -> String {
    zhconv(text, Variant::ZhTW)
}

/// 轉為簡體
pub fn to_simplified(text: &str) -> String {
    zhconv(text, Variant::ZhHans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_both_ways() {
        assert_eq!(to_traditional("简体中文"), "簡體中文");
        assert_eq!(to_simplified("繁體中文"), "繁体中文");
        // 非中文原樣保留
        assert_eq!(to_traditional("Hello 123"), "Hello 123");
    }
}
