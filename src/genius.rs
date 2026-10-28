//! Genius：抓原文頁，再找同一首歌的官方英文翻譯頁（Genius English Translations），
//! 逐段逐行合併成「一行原文、一行英譯」的中英對照格式。

use reqwest::Url;

use crate::bilingual::{self, has_cjk, section};
use crate::crawler::{self, SelectorOverrides, Song};
use crate::error::Result;
use crate::search::get_json;

/// 抓取結果：原始歌詞，加上可用時的中英對照版本
#[derive(Debug, Clone)]
pub struct Fetched {
    pub song: Song,
    /// 一行原文、一行英譯
    pub bilingual: Option<String>,
    /// 給使用者的說明，例如「Genius 沒有這首歌的英文翻譯」
    pub note: Option<String>,
}

impl From<Song> for Fetched {
    /// 非 Genius 來源：直接從歌詞本身偵測英譯（例如同一行附英文的網站）
    fn from(song: Song) -> Self {
        Self {
            bilingual: bilingual::to_bilingual(&song.lyrics),
            song,
            note: None,
        }
    }
}

pub fn is_genius_url(url: &str) -> bool {
    let host = crawler::host_of(url);
    host == "genius.com" || host.ends_with(".genius.com")
}

/// 依網址抓歌詞；Genius 會一併抓英文翻譯，其他網站同 `crawler::fetch_song`
pub async fn fetch_any(client: &reqwest::Client, url: &str, overrides: &SelectorOverrides) -> Result<Fetched> {
    if is_genius_url(url) {
        fetch(client, url, overrides).await
    } else {
        crawler::fetch_song(client, url, overrides).await.map(Fetched::from)
    }
}

pub(crate) enum Translation {
    /// 原文就是英文
    NotNeeded,
    Missing,
    Found(String),
}

/// 抓 Genius 原文頁與英文翻譯頁並合併。翻譯抓不到時只回報說明，不影響原文。
pub async fn fetch(client: &reqwest::Client, url: &str, overrides: &SelectorOverrides) -> Result<Fetched> {
    let (html, final_url) = crawler::fetch_html(client, url).await?;
    let song = crawler::parse_song(&html, &final_url, overrides)?;
    let plain = |song: Song, note: Option<String>| Fetched {
        bilingual: bilingual::to_bilingual(&song.lyrics),
        song,
        note,
    };

    let Some(id) = song_id(&html) else {
        return Ok(plain(song, None));
    };
    let tr_url = match english_translation(client, id).await {
        Ok(Translation::Found(u)) => u,
        Ok(Translation::NotNeeded) => return Ok(plain(song, None)),
        Ok(Translation::Missing) => return Ok(plain(song, Some("Genius 沒有這首歌的英文翻譯".into()))),
        Err(e) => return Ok(plain(song, Some(format!("讀取 Genius 英文翻譯失敗：{e}")))),
    };
    let tr = match crawler::fetch_song(client, &tr_url, &SelectorOverrides::default()).await {
        Ok(t) => t,
        Err(e) => return Ok(plain(song, Some(format!("抓取 Genius 英文翻譯失敗：{e}")))),
    };
    let (merged, complete) = merge(&song.lyrics, &tr.lyrics);
    let note = if complete {
        "已合併 Genius 英文翻譯".to_string()
    } else {
        "已合併 Genius 英文翻譯；部分段落英譯的斷行與原文不同，已依長度對齊，請在預覽中檢查".to_string()
    };
    Ok(Fetched {
        song,
        bilingual: Some(merged),
        note: Some(note),
    })
}

/// 頁面中的 `genius://songs/123` 取出歌曲 ID
fn song_id(html: &str) -> Option<u64> {
    const MARKER: &str = "genius://songs/";
    let start = html.find(MARKER)? + MARKER.len();
    let digits: String = html[start..].chars().take_while(char::is_ascii_digit).take(12).collect();
    digits.parse().ok()
}

pub(crate) async fn english_translation(client: &reqwest::Client, id: u64) -> Result<Translation> {
    let url = Url::parse(&format!("https://genius.com/api/songs/{id}")).expect("ID 只含數字");
    let v = get_json(client, url).await?;
    let song = &v["response"]["song"];
    if song["language"].as_str() == Some("en") {
        return Ok(Translation::NotNeeded);
    }
    let found = song["translation_songs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["language"].as_str() == Some("en"))
        .filter_map(|t| t["url"].as_str())
        // 只跟隨 Genius 自己的 https 網址
        .find(|u| u.starts_with("https://") && is_genius_url(u));
    Ok(found.map_or(Translation::Missing, |u| Translation::Found(u.to_string())))
}

struct Stanza<'a> {
    header: Option<&'a str>,
    lines: Vec<&'a str>,
}

/// 開頭的標題標記，例如「[歌手《歌名》歌詞]」「[Artist - Song (English Translation)]」
fn is_title_header(h: &str) -> bool {
    const MARKS: &[&str] = &["歌詞", "歌词", "가사", "Lyrics", "Translation", "Romanized", "Romanization"];
    MARKS.iter().any(|m| h.contains(m))
}

/// 依空行與段落標記切段；標題標記略過。
fn stanzas(text: &str) -> Vec<Stanza<'_>> {
    let mut out: Vec<Stanza> = Vec::new();
    let mut cur = Stanza { header: None, lines: Vec::new() };
    for line in text.lines().map(str::trim) {
        let header = section(line).filter(|h| !is_title_header(h));
        if line.is_empty() || section(line).is_some() {
            if !cur.lines.is_empty() {
                out.push(std::mem::replace(&mut cur, Stanza { header: None, lines: Vec::new() }));
            }
            if header.is_some() {
                cur.header = header;
            }
            continue;
        }
        cur.lines.push(line);
    }
    if !cur.lines.is_empty() {
        out.push(cur);
    }
    out
}

/// 「Verse 1: Karencici」→「Verse 1」
fn header_name(h: &str) -> &str {
    h.split([':', '：']).next().unwrap_or(h).trim()
}

/// 一行的「長度」：中日韓字元的資訊量約是英文字母的兩倍
fn weight(s: &str) -> f32 {
    let w: f32 = s
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if bilingual::is_cjk(c) { 2.0 } else { 1.0 })
        .sum();
    w.max(1.0)
}

/// 把一段的英譯行分配給原文行。行數相同時一對一；
/// 不同時（翻譯常把一行拆成兩行或合併）依累計長度比例分配，每行英譯只會分給一行原文。
fn align<'a>(orig: &[&str], tr: &[&'a str]) -> Vec<Vec<&'a str>> {
    let mut out = vec![Vec::new(); orig.len()];
    if orig.is_empty() {
        return out;
    }
    if orig.len() == tr.len() {
        for (o, t) in out.iter_mut().zip(tr) {
            o.push(*t);
        }
        return out;
    }
    let total_o: f32 = orig.iter().map(|s| weight(s)).sum();
    let total_t: f32 = tr.iter().map(|s| weight(s)).sum();
    let mut bounds = Vec::with_capacity(orig.len());
    let mut acc = 0.0;
    for s in orig {
        acc += weight(s) / total_o;
        bounds.push(acc);
    }
    let mut acc = 0.0;
    for t in tr {
        let w = weight(t) / total_t;
        // 以英譯行的中點落在哪一行原文的範圍內決定歸屬
        let mid = acc + w / 2.0;
        acc += w;
        let i = bounds.iter().position(|&b| mid <= b).unwrap_or(orig.len() - 1);
        out[i].push(*t);
    }
    out
}

/// 合併原文與英譯，回傳 (中英對照文字, 是否每段行數都相符)。
///
/// 段落數相同時逐段對齊；不同但總行數相同時依序對齊；都不同則逐段盡量對齊。
/// 段落標記優先使用英譯頁的英文名稱（資料庫以 `<Verse 1>` 表示）。
pub fn merge(original: &str, translation: &str) -> (String, bool) {
    let orig = stanzas(original);
    let tr = stanzas(translation);
    let orig_total: usize = orig.iter().map(|s| s.lines.len()).sum();
    let tr_total: usize = tr.iter().map(|s| s.lines.len()).sum();
    let same_structure = orig.len() == tr.len();

    // 每段對應的英譯行
    let per_stanza: Vec<Vec<&str>> = if !same_structure && orig_total == tr_total {
        let mut flat = tr.iter().flat_map(|s| s.lines.iter().copied());
        orig.iter().map(|s| flat.by_ref().take(s.lines.len()).collect()).collect()
    } else {
        (0..orig.len())
            .map(|i| tr.get(i).map(|t| t.lines.clone()).unwrap_or_default())
            .collect()
    };
    let complete = orig.iter().zip(&per_stanza).all(|(s, t)| s.lines.len() == t.len());

    let mut out: Vec<String> = Vec::new();
    for (i, (stanza, tr_lines)) in orig.iter().zip(&per_stanza).enumerate() {
        if !out.is_empty() {
            out.push(String::new());
        }
        let tr_header = same_structure.then(|| tr[i].header).flatten();
        if let Some(h) = tr_header.or(stanza.header) {
            out.push(format!("<{}>", header_name(h)));
        }
        for (line, parts) in stanza.lines.iter().zip(align(&stanza.lines, tr_lines)) {
            out.push((*line).to_string());
            let t = parts.join(" ");
            // 原文本身是英文（或翻譯與原文相同）就不重複一行
            if !t.is_empty() && has_cjk(line) && !t.eq_ignore_ascii_case(line) {
                out.push(t);
            }
        }
    }
    (out.join("
"), complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bilingual::{Line, classify};

    #[test]
    fn extracts_song_id() {
        assert_eq!(song_id(r#"<meta content="genius://songs/11404920" />"#), Some(11404920));
        assert_eq!(song_id("<html>"), None);
        assert!(is_genius_url("https://genius.com/a"));
        assert!(!is_genius_url("https://evilgenius.com/a"));
    }

    #[test]
    fn merges_by_stanza_with_english_headers() {
        let orig = "[歌手《歌名》歌詞]\n\n[主歌 1: 歌手]\n早安\n今天也要加油\nHello hello\n\n[副歌]\n天氣很好";
        let tr = "[Artist - Song (English Translation)]\n\n[Verse 1: Artist]\nGood morning\nKeep going today\nHello hello\n\n[Chorus]\nThe weather is nice";
        let (merged, complete) = merge(orig, tr);
        assert!(complete);
        assert_eq!(
            merged,
            "<Verse 1>\n早安\nGood morning\n今天也要加油\nKeep going today\nHello hello\n\n<Chorus>\n天氣很好\nThe weather is nice"
        );
        // 合併結果交給 Notion 時分類正確
        let lines = classify(&merged);
        assert_eq!(lines[1], Line::Lyric("早安"));
        assert_eq!(lines[2], Line::Translation("Good morning"));
        assert_eq!(lines[5], Line::Lyric("Hello hello"));
    }

    #[test]
    fn aligns_rebroken_lines_by_length() {
        let parts = align(&["早安 今天也要加油", "天氣很好"], &["Good morning", "Keep going today", "The weather is nice"]);
        assert_eq!(parts, [vec!["Good morning", "Keep going today"], vec!["The weather is nice"]]);
        let parts = align(&["一二三四五", "六七八九十"], &["one two three four five six seven eight nine ten"]);
        assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), 1);
    }

    #[test]
    fn falls_back_to_line_order_and_flags_mismatch() {
        // 段落切法不同但總行數相同：依序對齊
        let (merged, complete) = merge("一\n二\n\n三", "One\nTwo\nThree");
        assert!(complete);
        assert_eq!(merged, "一\nOne\n二\nTwo\n\n三\nThree");

        // 行數不同：盡量對齊並回報
        let (merged, complete) = merge("一\n二", "One");
        assert!(!complete);
        assert_eq!(merged, "一\nOne\n二");
    }
}
