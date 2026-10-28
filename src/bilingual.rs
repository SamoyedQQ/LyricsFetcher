//! 中英對照歌詞。
//!
//! 文字格式：一行原文、下一行英譯，段落之間空一行，`<Verse 1>` 這類為段落標記。
//! 寫入 Notion 時原文為 Heading 2、英譯為橘色粗體段落（與既有資料庫的格式一致）。

/// 有英譯的原文行占比達此值才視為中英對照
const MIN_PAIRED_RATIO: f32 = 0.5;
/// 至少要有幾行英譯，避免短歌詞誤判
const MIN_PAIRS: usize = 4;

#[derive(Debug, PartialEq, Eq)]
pub enum Line<'a> {
    /// 段落分隔
    Blank,
    /// 段落標記，不含括號，例如 "Verse 1"
    Section(&'a str),
    /// 原文
    Lyric(&'a str),
    /// 緊接在原文後的英譯
    Translation(&'a str),
}

/// 中日韓文字與全形標點
pub(crate) fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3000..=0x31FF | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// `[Verse 1]`、`<Chorus>` → 括號內文字
pub(crate) fn section(line: &str) -> Option<&str> {
    let inner = line
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .or_else(|| line.strip_prefix('<').and_then(|s| s.strip_suffix('>')))?
        .trim();
    (!inner.is_empty() && inner.chars().count() <= 40 && !inner.contains(['[', ']', '<', '>'])).then_some(inner)
}

/// 漢語拼音行（中、拼音、英三行對照的網站會出現）
fn is_pinyin(line: &str) -> bool {
    const TONES: &str = "āáǎàēéěèīíǐìōóǒòūúǔùǖǘǚǜ";
    line.chars().filter(|c| TONES.contains(*c)).count() >= 2
}

/// 「原文 English」同一行時，從最後一個中日韓字元後切開
fn split_line(line: &str) -> (&str, Option<&str>) {
    let Some((i, c)) = line.char_indices().filter(|&(_, c)| is_cjk(c)).last() else {
        return (line, None);
    };
    let end = i + c.len_utf8();
    let rest = line[end..].trim();
    if rest.chars().filter(char::is_ascii_alphabetic).count() >= 2 {
        (line[..end].trim_end(), Some(rest))
    } else {
        (line, None)
    }
}

/// 偵測歌詞是否附英譯；是的話轉成「一行原文、一行英譯」的格式，否則回傳 None。
///
/// 支援同一行（`早安 Good morning`）與分行（原文 / 拼音 / 英譯）兩種來源格式。
pub fn to_bilingual(text: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    let (mut cjk_lines, mut paired) = (0usize, 0usize);
    // 上一行原文還沒有英譯
    let mut open = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            if out.last().is_some_and(|l| !l.is_empty()) {
                out.push(String::new());
            }
            open = false;
        } else if let Some(s) = section(line) {
            out.push(format!("<{s}>"));
            open = false;
        } else if has_cjk(line) {
            cjk_lines += 1;
            let (lyric, tr) = split_line(line);
            out.push(lyric.to_string());
            open = tr.is_none();
            if let Some(t) = tr {
                out.push(t.to_string());
                paired += 1;
            }
        } else if is_pinyin(line) {
            continue;
        } else {
            if open {
                paired += 1;
            }
            out.push(line.to_string());
            open = false;
        }
    }
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    let enough = paired >= MIN_PAIRS && paired as f32 >= cjk_lines as f32 * MIN_PAIRED_RATIO;
    enough.then(|| out.join("\n"))
}

/// 左右對照的頁面（LyricsTranslate）：每列為 (段落編號, 原文, 譯文)。
/// 回傳 (只有原文的歌詞, 中英對照版本)；譯文不足時中英版本為 None。
pub fn from_columns(rows: &[(u32, String, String)]) -> (String, Option<String>) {
    // 欄位順序以內容判斷：含中日韓文字較多的一欄是原文
    let cjk = |pick: fn(&(u32, String, String)) -> &str| rows.iter().filter(|r| has_cjk(pick(r))).count();
    let swap = cjk(|r| &r.2) > cjk(|r| &r.1);
    let (mut plain, mut both): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut paired = 0;
    let mut last_par = None;
    for (par, a, b) in rows {
        let (orig, tr) = if swap { (b.trim(), a.trim()) } else { (a.trim(), b.trim()) };
        if orig.is_empty() {
            continue;
        }
        if last_par.is_some_and(|p| p != *par) {
            plain.push(String::new());
            both.push(String::new());
        }
        last_par = Some(*par);
        plain.push(orig.to_string());
        both.push(orig.to_string());
        if !tr.is_empty() && has_cjk(orig) && !tr.eq_ignore_ascii_case(orig) {
            both.push(tr.to_string());
            paired += 1;
        }
    }
    let both = (paired >= MIN_PAIRS).then(|| both.join("\n"));
    (plain.join("\n"), both)
}

/// 左右對照頁同一行在頁面上的所有文字 → (原文, 譯文)。
///
/// 原文欄可能缺少、在 DOM 中排在譯文之後、重複出現或只有拼音，不能只看順序：
/// 含中日韓文字的是原文，拼音略過，其餘第一個不同的文字是譯文。
pub fn pick_columns(texts: &[String]) -> (String, String) {
    let texts: Vec<&str> = texts.iter().map(|t| t.trim()).filter(|t| !t.is_empty() && !is_pinyin(t)).collect();
    let orig = texts.iter().find(|t| has_cjk(t)).or(texts.first()).copied().unwrap_or_default();
    let tr = texts.iter().find(|t| **t != orig && !has_cjk(t)).copied().unwrap_or_default();
    (orig.to_string(), tr.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 原文（中文，或其他中日韓語言）
    Original,
    English,
    /// 已是中英對照
    Both,
}

/// 判斷一份歌詞是原文、英譯，還是已附英譯
pub fn lang_of(lyrics: &str, bilingual: Option<&str>) -> Lang {
    if bilingual.is_some() || to_bilingual(lyrics).is_some() {
        return Lang::Both;
    }
    let cjk = lyrics.chars().filter(|c| is_cjk(*c)).count();
    let latin = lyrics.chars().filter(char::is_ascii_alphabetic).count();
    // 一個中文字約等於四、五個英文字母；中文歌詞夾雜英文段落仍算原文
    if cjk > 0 && cjk * 4 >= latin { Lang::Original } else { Lang::English }
}

/// 一份抓到的歌詞：原文與可用時的中英對照版本
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub lyrics: &'a str,
    pub bilingual: Option<&'a str>,
}

#[derive(Debug)]
pub struct Combined {
    /// 歌名、歌手、來源網址取自哪一份（原文那份）
    pub primary: usize,
    pub lyrics: String,
    pub bilingual: Option<String>,
    pub note: String,
}

/// 任選兩份歌詞合併成中英對照：自動判斷哪份是原文、哪份是英譯。
/// 兩份語言重複（都是原文、都是英譯，或其中一份已含中英）時去重，只保留一份。
pub fn combine(sources: [Source; 2]) -> Combined {
    let langs = sources.map(|s| lang_of(s.lyrics, s.bilingual));
    let keep = |i: usize, note: &str| Combined {
        primary: i,
        lyrics: sources[i].lyrics.to_string(),
        bilingual: sources[i].bilingual.map(str::to_string).or_else(|| to_bilingual(sources[i].lyrics)),
        note: note.to_string(),
    };
    match langs {
        [Lang::Original, Lang::English] | [Lang::English, Lang::Original] => {
            let (orig, tr) = if langs[0] == Lang::Original { (0, 1) } else { (1, 0) };
            let (merged, complete) = crate::genius::merge(sources[orig].lyrics, sources[tr].lyrics);
            Combined {
                primary: orig,
                lyrics: sources[orig].lyrics.to_string(),
                bilingual: Some(merged),
                note: if complete {
                    "已合併原文與英文翻譯".into()
                } else {
                    "已合併原文與英文翻譯；部分段落行數不同，請檢查對齊".into()
                },
            }
        }
        [Lang::Original, Lang::Original] => keep(0, "兩個都是原文（重複），只保留第一個；請改選一個英文翻譯"),
        [Lang::English, Lang::English] => keep(0, "兩個都是英文翻譯（重複），只保留第一個；請改選一個原文歌詞"),
        [Lang::Both, Lang::Both] => keep(0, "兩個都已是中英對照（重複），只保留第一個"),
        [Lang::Both, _] => keep(0, "第一個已含中英對照，第二個是重複的版本，已略過"),
        [_, Lang::Both] => keep(1, "第二個已含中英對照，第一個是重複的版本，已略過"),
    }
}

/// 把中英對照格式的文字逐行分類（寫入 Notion 用）。
///
/// 含中日韓文字的行是原文；緊接在原文後、不含中日韓文字的行是英譯；
/// 其餘英文行視為原文（英文歌詞本身不需要翻譯）。
pub fn classify(text: &str) -> Vec<Line<'_>> {
    let mut out = Vec::new();
    let mut open = false;
    for line in text.lines().map(str::trim) {
        let item = if line.is_empty() {
            // 連續空行與開頭空行沒有意義
            if matches!(out.last(), None | Some(Line::Blank)) {
                continue;
            }
            open = false;
            Line::Blank
        } else if let Some(s) = section(line) {
            open = false;
            Line::Section(s)
        } else if open && !has_cjk(line) {
            open = false;
            Line::Translation(line)
        } else {
            open = has_cjk(line);
            Line::Lyric(line)
        };
        out.push(item);
    }
    while out.last() == Some(&Line::Blank) {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_line_pairs() {
        let src = "週末 我們去海邊 This weekend let's go to the beach\n\
                   帶上 一本 舊的小說 Bring an old novel along\n\
                   So 我的朋友 So, my friend\n\
                   So what you say？\n\
                   要不要一起出發？Do you want to set off together?\n\n\
                   天氣很好 The weather is nice";
        let out = to_bilingual(src).unwrap();
        assert_eq!(
            out,
            "週末 我們去海邊\nThis weekend let's go to the beach\n\
             帶上 一本 舊的小說\nBring an old novel along\n\
             So 我的朋友\nSo, my friend\n\
             So what you say？\n\
             要不要一起出發？\nDo you want to set off together?\n\n\
             天氣很好\nThe weather is nice"
        );
    }

    #[test]
    fn separate_lines_with_pinyin() {
        let src = "[Verse 1]\n天氣很好\ntiān qì hěn hǎo\nThe weather is nice\n出發\nchū fā\nLet's go\n\
                   愛你\nài nǐ\nLove you\n錯過\ncuò guò\nMissed";
        let out = to_bilingual(src).unwrap();
        assert!(out.starts_with("<Verse 1>\n天氣很好\nThe weather is nice\n出發\nLet's go"));
        assert!(!out.contains("hǎo"));
    }

    #[test]
    fn monolingual_is_not_converted() {
        assert_eq!(to_bilingual("窗外的雨停了\n桌上的咖啡 latte\n\n街角的燈 慢慢亮起"), None);
        assert_eq!(to_bilingual("just\nenglish\nlyrics\nhere\nonly"), None);
    }

    #[test]
    fn columns_to_bilingual() {
        let row = |p: u32, a: &str, b: &str| (p, a.to_string(), b.to_string());
        let rows = [
            row(0, "第一行", "Line one"),
            row(0, "第二行", "Line two"),
            row(0, "", ""),
            row(1, "第三行", "Line three"),
            row(1, "Oh oh", "Oh oh"),
            row(1, "第四行", "Line four"),
        ];
        let (plain, both) = from_columns(&rows);
        assert_eq!(plain, "第一行\n第二行\n\n第三行\nOh oh\n第四行");
        assert_eq!(
            both.as_deref(),
            Some("第一行\nLine one\n第二行\nLine two\n\n第三行\nLine three\nOh oh\n第四行\nLine four")
        );
        // 欄位左右相反時自動對調
        let swapped: Vec<_> = rows.iter().map(|(p, a, b)| (*p, b.clone(), a.clone())).collect();
        assert_eq!(from_columns(&swapped).1, both);
    }

    #[test]
    fn picks_columns_by_content() {
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // 譯文排在前面、拼音、重複出現都能分辨
        assert_eq!(pick_columns(&v(&["Line one", "第一行"])), ("第一行".into(), "Line one".into()));
        assert_eq!(pick_columns(&v(&["dì yī háng", "Line one"])), ("Line one".into(), String::new()));
        assert_eq!(pick_columns(&v(&["第一行", "第一行", "Line one"])), ("第一行".into(), "Line one".into()));
        assert_eq!(pick_columns(&v(&["Oh oh", "Oh oh"])), ("Oh oh".into(), String::new()));
    }

    #[test]
    fn combines_original_and_english() {
        let zh = "第一行
第二行

第三行";
        let en = "Line one
Line two

Line three";
        let src = |lyrics| Source { lyrics, bilingual: None };
        // 順序不影響：原文那份為主
        let c = combine([src(en), src(zh)]);
        assert_eq!(c.primary, 1);
        assert_eq!(c.lyrics, zh);
        assert_eq!(c.bilingual.as_deref(), Some("第一行
Line one
第二行
Line two

第三行
Line three"));

        // 語言重複：去重
        let c = combine([src(zh), src(zh)]);
        assert_eq!((c.primary, c.bilingual), (0, None));
        let both = "第一行
Line one
第二行
Line two
第三行
Line three
第四行
Line four";
        let c = combine([src(zh), src(both)]);
        assert_eq!(c.primary, 1);
        assert!(c.bilingual.is_some());
    }

    #[test]
    fn classifies_lines() {
        let lines = classify("\n<Chorus>\n天氣很好\nThe weather is nice\nOh baby\nyeah yeah\n\n\n走吧\n\n");
        assert_eq!(
            lines,
            [
                Line::Section("Chorus"),
                Line::Lyric("天氣很好"),
                Line::Translation("The weather is nice"),
                Line::Lyric("Oh baby"),
                Line::Lyric("yeah yeah"),
                Line::Blank,
                Line::Lyric("走吧"),
            ]
        );
    }
}
