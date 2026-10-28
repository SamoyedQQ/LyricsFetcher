//! 抓取歌詞網頁並萃取歌名、歌手、歌詞。
//!
//! 解析優先順序（每個欄位各自獨立）：
//!   1. 環境變數指定的 CSS selector（TITLE_SELECTOR / ARTIST_SELECTOR / LYRICS_SELECTOR）
//!   2. 內建網站設定檔（依網域比對）
//!   3. 通用規則（og:title、常見的 lyrics class / id 等）

use std::sync::OnceLock;
use std::time::Duration;

use reqwest::header::{self, HeaderMap, HeaderValue};
use scraper::{ElementRef, Html, Node, Selector};

use crate::bilingual::is_cjk;
use crate::error::{AppError, Result};

const CHROME_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// 網頁大小上限：一般歌詞頁 < 1 MiB，超過即視為異常，避免記憶體被耗盡
const MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub lyrics: String,
    pub source_url: String,
}

/// 建立共用的 HTTP 客戶端（爬蟲與 Notion 共用連線池）。
pub fn build_client() -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT_LANGUAGE,
        HeaderValue::from_static("zh-TW,zh;q=0.9,en-US;q=0.8,en;q=0.7"),
    );
    Ok(reqwest::Client::builder()
        .user_agent(CHROME_UA)
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()?)
}

/// 只接受 http(s) 網址
pub fn validate_url(url: &str) -> Result<()> {
    match reqwest::Url::parse(url.trim()) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() => Ok(()),
        _ => Err(AppError::Usage(format!("網址必須以 http:// 或 https:// 開頭：{url}"))),
    }
}

pub async fn fetch_song(client: &reqwest::Client, url: &str, overrides: &SelectorOverrides) -> Result<Song> {
    let (html, final_url) = fetch_html(client, url).await?;
    parse_song(&html, &final_url, overrides)
}

/// 下載並轉碼網頁，回傳 (HTML, 跟隨轉址後的最終網址)
pub(crate) async fn fetch_html(client: &reqwest::Client, url: &str) -> Result<(String, String)> {
    validate_url(url)?;
    let mut resp = client
        .get(url)
        .header(
            header::ACCEPT,
            "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8",
        )
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Upgrade-Insecure-Requests", "1")
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Http {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }

    let too_large = || AppError::Parse(format!("網頁超過 {} MiB 上限，已中止下載", MAX_PAGE_BYTES >> 20));
    if resp.content_length().is_some_and(|n| n > MAX_PAGE_BYTES as u64) {
        return Err(too_large());
    }
    let header_charset = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(charset_from_content_type)
        .map(str::to_owned);
    // 以最終網址（跟隨轉址後）判斷網站
    let final_url = resp.url().to_string();

    // 串流讀取並設上限：Content-Length 可能缺少或不實，不能只靠它
    let mut body = Vec::with_capacity(256 * 1024);
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > MAX_PAGE_BYTES {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }

    Ok((decode(&body, header_charset.as_deref()), final_url))
}

/// 依 HTTP header → `<meta charset>` → UTF-8 的順序決定編碼。
fn decode(body: &[u8], header_charset: Option<&str>) -> String {
    let label = header_charset.map(str::to_owned).or_else(|| sniff_meta_charset(body));
    let encoding = label
        .and_then(|l| encoding_rs::Encoding::for_label(l.as_bytes()))
        .unwrap_or(encoding_rs::UTF_8);
    encoding.decode(body).0.into_owned()
}

fn charset_from_content_type(ct: &str) -> Option<&str> {
    ct.split(';')
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, v)| v.trim().trim_matches('"'))
}

/// 只掃描前 2 KiB，對應 HTML 規範的 meta charset 預掃描
fn sniff_meta_charset(body: &[u8]) -> Option<String> {
    let head = &body[..body.len().min(2048)];
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    let start = head.find("charset=")? + "charset=".len();
    let label: String = head[start..]
        .trim_start_matches(['"', '\''])
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    (!label.is_empty()).then_some(label)
}

// ─── 解析 ───────────────────────────────────────────────────────────────

/// 使用者自訂的 CSS selector，優先於內建規則
#[derive(Debug, Default, Clone)]
pub struct SelectorOverrides {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub lyrics: Option<String>,
}

impl SelectorOverrides {
    /// 從 TITLE_SELECTOR / ARTIST_SELECTOR / LYRICS_SELECTOR 讀取並驗證
    pub fn from_env() -> Result<Self> {
        let get = |k| std::env::var(k).ok();
        Self::new(get("TITLE_SELECTOR"), get("ARTIST_SELECTOR"), get("LYRICS_SELECTOR"))
    }

    /// 空字串視為未設定；語法錯誤的 selector 直接回報，而不是默默略過
    pub fn new(title: Option<String>, artist: Option<String>, lyrics: Option<String>) -> Result<Self> {
        let check = |field: &str, v: Option<String>| -> Result<Option<String>> {
            let Some(v) = v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) else {
                return Ok(None);
            };
            Selector::parse(&v)
                .map(|_| Some(v.clone()))
                .map_err(|e| AppError::Config(format!("{field} 不是有效的 CSS selector：`{v}`（{e:?}）")))
        };
        Ok(Self {
            title: check("歌名 selector", title)?,
            artist: check("歌手 selector", artist)?,
            lyrics: check("歌詞 selector", lyrics)?,
        })
    }
}

/// 從 `<title>` 解析出 (歌名, 歌手)
type TitleTagParser = fn(&str) -> Option<(String, String)>;

struct SiteProfile {
    /// 網域結尾，例如 "genius.com"
    host: &'static str,
    title: &'static [&'static str],
    artist: &'static [&'static str],
    lyrics: &'static [&'static str],
    title_tag: Option<TitleTagParser>,
}

/// 內建的網站設定檔。網站改版時 selector 可能失效，可用環境變數覆蓋。
const PROFILES: &[SiteProfile] = &[
    SiteProfile {
        host: "genius.com",
        title: &[],
        artist: &[],
        lyrics: &[r#"[data-lyrics-container="true"]"#],
        // 例："Artist – Title Lyrics | Genius Lyrics"
        title_tag: Some(|t| {
            let t = t.split(" | ").next()?.trim();
            let t = t.strip_suffix(" Lyrics").unwrap_or(t);
            let (artist, title) = t.split_once(" \u{2013} ")?;
            Some((title.trim().into(), artist.trim().into()))
        }),
    },
    SiteProfile {
        host: "mojim.com",
        title: &["#fsZx2"],
        artist: &["#fsZx1 > a", "dl.fsZx1 a"],
        lyrics: &["#fsZx3"],
        // 例："歌名 歌詞 歌手 ※ Mojim.com 魔鏡歌詞網"
        title_tag: Some(|t| {
            let t = t.split('※').next()?.trim();
            let (title, artist) = t.split_once(" 歌詞 ")?;
            Some((title.trim().into(), artist.trim().into()))
        }),
    },
    // KKBOX 有 AWS WAF 人機驗證，一般 HTTP 請求拿不到內容；
    // GUI 會改用內嵌瀏覽器擷取，這組 selector 供擷取結果與日後可直接存取時共用
    SiteProfile {
        host: "kkbox.com",
        title: &["main h1", "h1"],
        artist: &[r#"main a[href*="/artist/"]"#],
        lyrics: &[".track-content .lyrics", "div.lyrics"],
        title_tag: None,
    },
    SiteProfile {
        host: "asianpopweekly.com",
        title: &[],
        artist: &[],
        lyrics: &[".post-content"],
        title_tag: Some(asianpopweekly_title),
    },
    SiteProfile {
        host: "azlyrics.com",
        title: &["div.ringtone ~ b"],
        artist: &["div.lyricsh h2 b"],
        lyrics: &["div.ringtone ~ div:not([class])"],
        title_tag: None,
    },
];

const GENERIC_TITLE: &[&str] = &[r#"meta[property="og:title"]"#, r#"[itemprop="name"]"#, "h1"];
const GENERIC_ARTIST: &[&str] = &[
    r#"meta[name="artist"]"#,
    r#"[itemprop="byArtist"] [itemprop="name"]"#,
    r#"[itemprop="byArtist"]"#,
    ".artist",
    ".singer",
    r#"[class*="artist"]"#,
];
/// 依「明確 → 寬鬆」排序；先命中有意義內容的就採用
const GENERIC_LYRICS: &[&str] = &[
    r#"[data-lyrics-container="true"]"#,
    "#lyrics",
    "#lyric",
    ".lyrics",
    ".lyric",
    ".lyrics-content",
    r#"[itemprop="text"]"#,
    r#"[id*="lyric"]"#,
    r#"[class*="lyric"]"#,
];

/// 行內含有即移除（網站浮水印）
const JUNK_SUBSTRINGS: &[&str] = &["Mojim.com", "魔鏡歌詞網", "更多更詳盡歌詞"];
/// 整行完全相符才移除，避免誤刪真正的歌詞
const JUNK_LINES: &[&str] = &["Embed", "You might also like"];

const MIN_LYRICS_CHARS: usize = 20;

/// 解析 HTML。歌名可能為空字串（交由呼叫端搭配 `--title` 判斷）；找不到歌詞則回傳錯誤。
pub fn parse_song(html: &str, url: &str, overrides: &SelectorOverrides) -> Result<Song> {
    let doc = Html::parse_document(html);
    let host = host_of(url);
    let profile = PROFILES
        .iter()
        .find(|p| host == p.host || host.ends_with(&format!(".{}", p.host)));
    let tag_pair = profile
        .and_then(|p| p.title_tag)
        .zip(first_text(&doc, &["title"]))
        .and_then(|(parse, t)| parse(&t));

    let site = |f: fn(&SiteProfile) -> &'static [&'static str]| profile.map(f).unwrap_or(&[]);
    let pick = |custom: &Option<String>, site: &[&str], from_tag: Option<&String>, generic: &[&str]| {
        custom
            .as_deref()
            .and_then(|sel| first_text(&doc, &[sel]))
            .or_else(|| first_text(&doc, site))
            .or_else(|| from_tag.cloned())
            .or_else(|| first_text(&doc, generic))
    };

    let title = pick(
        &overrides.title,
        site(|p| p.title),
        tag_pair.as_ref().map(|(t, _)| t),
        GENERIC_TITLE,
    )
    .map(|t| clean_title(&t))
    .unwrap_or_default();

    let artist = pick(
        &overrides.artist,
        site(|p| p.artist),
        tag_pair.as_ref().map(|(_, a)| a),
        GENERIC_ARTIST,
    )
    .map(|a| clean_artist(&a))
    .unwrap_or_default();

    let lyrics = overrides
        .lyrics
        .as_deref()
        .and_then(|sel| lyrics_text(&doc, &[sel]))
        .or_else(|| lyrics_text(&doc, site(|p| p.lyrics)))
        .or_else(|| lyrics_text(&doc, GENERIC_LYRICS))
        .ok_or_else(|| {
            AppError::Parse(
                "找不到歌詞區塊（若網站以 JavaScript 動態載入歌詞則無法直接抓取）。\n    \
                 可在 .env 設定 LYRICS_SELECTOR 指定歌詞所在元素的 CSS selector"
                    .into(),
            )
        })?;

    Ok(Song {
        title,
        artist,
        lyrics,
        source_url: url.to_string(),
    })
}

pub(crate) fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    host.to_ascii_lowercase()
}

/// 內建 selector 皆為常數；使用者 selector 已在 `SelectorOverrides::new` 驗證過
fn compile(sel: &str) -> Option<Selector> {
    Selector::parse(sel).ok()
}

/// 回傳第一個有文字內容的元素（<meta> 取 content 屬性），只取第一行。
fn first_text(doc: &Html, selectors: &[&str]) -> Option<String> {
    selectors.iter().filter_map(|s| compile(s)).find_map(|sel| {
        doc.select(&sel).find_map(|el| {
            let raw = match el.value().attr("content") {
                Some(c) if el.value().name() == "meta" => c.to_string(),
                _ => extract_text(el),
            };
            raw.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(str::to_string)
        })
    })
}

/// 依序嘗試 selector；同一個 selector 的多個命中會串接（Genius 會把歌詞拆成數個容器）。
fn lyrics_text(doc: &Html, selectors: &[&str]) -> Option<String> {
    selectors.iter().filter_map(|s| compile(s)).find_map(|sel| {
        let parts: Vec<String> = doc.select(&sel).map(extract_text).collect();
        let joined = normalize_lyrics(&parts.join("\n\n"));
        (joined.chars().count() >= MIN_LYRICS_CHARS).then_some(joined)
    })
}

/// 走訪 DOM 時的待辦項目
enum Item<'a> {
    Enter(ElementRef<'a>, bool),
    Text(&'a str, bool),
    Newline,
}

/// 把元素轉成純文字並保留換行：`<br>` 與區塊元素轉成 `\n`。
///
/// 若元素內完全沒有 `<br>` / 區塊元素，代表網站靠 CSS `white-space: pre` 排版，
/// 此時保留原始換行；否則比照瀏覽器把原始碼中的空白摺疊成單一空格。
///
/// 使用明確堆疊而非遞迴：惡意的深層巢狀 HTML 不會造成堆疊溢位。
fn extract_text(root: ElementRef<'_>) -> String {
    static STRUCTURE: OnceLock<Selector> = OnceLock::new();
    let structure = STRUCTURE.get_or_init(|| Selector::parse("br, p, div, li").expect("static selector"));
    let preserve = root.value().name() == "pre" || root.select(structure).next().is_none();

    let mut out = String::new();
    let mut stack = vec![Item::Enter(root, preserve)];
    let mut children = Vec::new();

    while let Some(item) = stack.pop() {
        let (el, preserve) = match item {
            Item::Newline => {
                out.push('\n');
                continue;
            }
            Item::Text(t, true) => {
                out.push_str(t);
                continue;
            }
            Item::Text(t, false) => {
                push_collapsed(&mut out, t);
                continue;
            }
            Item::Enter(el, preserve) => (el, preserve),
        };

        // 子節點要反向壓入堆疊，彈出時才會是文件順序
        children.clear();
        for child in el.children() {
            match child.value() {
                Node::Text(t) => children.push(Item::Text(t, preserve)),
                Node::Element(e) => {
                    let name = e.name();
                    if name == "br" {
                        children.push(Item::Newline);
                        continue;
                    }
                    if is_skipped(name) || e.attr("data-exclude-from-selection").is_some() {
                        continue;
                    }
                    let Some(child_el) = ElementRef::wrap(child) else { continue };
                    let block = is_block(name);
                    if block {
                        children.push(Item::Newline);
                    }
                    children.push(Item::Enter(child_el, preserve || name == "pre"));
                    if block {
                        children.push(Item::Newline);
                    }
                }
                _ => {}
            }
        }
        stack.extend(children.drain(..).rev());
    }
    out
}

fn is_skipped(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "template" | "button" | "svg" | "iframe" | "form" | "head"
    )
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "li" | "ul" | "ol" | "section" | "article" | "h1" | "h2" | "h3" | "h4" | "h5"
            | "h6" | "dd" | "dt" | "tr" | "pre" | "blockquote"
    )
}

fn push_collapsed(out: &mut String, text: &str) {
    let mut prev_space = out.is_empty() || out.ends_with([' ', '\n']);
    for c in text.chars() {
        // 全形空白 (U+3000) 常被當作歌詞排版用，保留
        if c.is_whitespace() && c != '\u{3000}' {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(c);
            prev_space = false;
        }
    }
}

/// 整理歌詞：逐行 trim、移除浮水印、LRC 時間軸與開頭的製作資訊，連續空行壓成一行。
/// 可重複套用（冪等），手動貼上的文字也適用。
pub fn clean_lyrics(raw: &str) -> String {
    normalize_lyrics(raw)
}

/// 從 HTML 片段（例如瀏覽器擷取到的歌詞容器 outerHTML）取出整理過的歌詞
pub fn lyrics_from_fragment(html: &str) -> String {
    let doc = Html::parse_fragment(html);
    normalize_lyrics(&extract_text(doc.root_element()))
}

fn normalize_lyrics(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut blank_pending = false;
    for line in raw.lines() {
        let line = line.trim_matches(|c: char| c.is_whitespace() && c != '\u{3000}');
        if line.is_empty() {
            blank_pending = !out.is_empty();
            continue;
        }
        // 只移除「開頭」的製作資訊，歌詞中間出現相同字樣不受影響
        if is_junk(line) || (out.is_empty() && is_credit(line)) {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank_pending {
                out.push('\n');
            }
        }
        blank_pending = false;
        out.push_str(line);
    }
    out
}

fn is_junk(line: &str) -> bool {
    JUNK_LINES.contains(&line) || JUNK_SUBSTRINGS.iter().any(|m| line.contains(m)) || is_lrc_tag(line)
}

/// 製作資訊行，例如「作詞：X　作曲：Y」「Composer: X」
fn is_credit(line: &str) -> bool {
    const KEYS: &[&str] = &[
        "作詞", "作曲", "編曲", "監製", "製作人", "製作", "詞", "曲", "演唱", "原唱", "OP", "SP", "ISRC",
        "Lyricist", "Lyrics by", "Lyrics", "Music", "Composer", "Composed by", "Arranger", "Arranged by", "Producer", "Produced by",
    ];
    KEYS.iter().any(|k| {
        line.strip_prefix(k)
            .map(|rest| rest.trim_start())
            .is_some_and(|rest| rest.starts_with([':', '：', '/']))
    })
}

/// LRC 時間軸 / 標籤行：`[00:12.34]...`、`[ti:xxx]`
fn is_lrc_tag(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else { return false };
    let Some((tag, _)) = rest.split_once(']') else { return false };
    let Some((key, _)) = tag.split_once(':') else { return false };
    !key.is_empty() && key.len() <= 6 && key.bytes().all(|b| b.is_ascii_alphanumeric())
}

pub fn clean_title(t: &str) -> String {
    let t = t.trim().trim_matches(|c| matches!(c, '"' | '“' | '”' | '「' | '」'));
    let t = strip_any_suffix(t, &[" - KKBOX", " | KKBOX", "｜KKBOX"]);
    strip_any_suffix(t, &[" lyrics", " Lyrics", " 歌詞"]).to_string()
}

/// 「[Lyric Translation] 歌手 – English Title 中文歌名 - Asian Pop Weekly」→ ("中文歌名 (English Title)", 歌手)。
/// 不是歌詞翻譯文章時回傳 None。
pub fn asianpopweekly_title(t: &str) -> Option<(String, String)> {
    let t = strip_any_suffix(t.trim(), &[" - Asian Pop Weekly", " – Asian Pop Weekly"]);
    let rest = ["[Lyric Translation]", "[Lyrics Translation]"]
        .iter()
        .find_map(|p| t.strip_prefix(p))?
        .trim();
    let (artist, title) = rest.split_once(" – ").or_else(|| rest.split_once(" - "))?;
    Some((bilingual_title(title.trim()), artist.trim().to_string()))
}

/// 「English Title 中文歌名」→「中文歌名 (English Title)」，與資料庫既有歌名格式一致
fn bilingual_title(t: &str) -> String {
    let cjk: Vec<usize> = t.char_indices().filter(|&(_, c)| is_cjk(c)).map(|(i, _)| i).collect();
    let (Some(&first), Some(&last)) = (cjk.first(), cjk.last()) else {
        return t.to_string();
    };
    let last_end = last + t[last..].chars().next().map_or(0, char::len_utf8);
    let (native, english) = if first == 0 {
        (&t[..last_end], &t[last_end..])
    } else {
        (&t[first..], &t[..first])
    };
    let (native, english) = (native.trim(), english.trim());
    if english.is_empty() || english.contains('(') {
        t.to_string()
    } else {
        format!("{native} ({english})")
    }
}

pub fn clean_artist(a: &str) -> String {
    strip_any_suffix(a.trim(), &[" Lyrics", " lyrics"]).to_string()
}

fn strip_any_suffix<'a>(s: &'a str, suffixes: &[&str]) -> &'a str {
    suffixes
        .iter()
        .find_map(|suf| s.strip_suffix(suf))
        .unwrap_or(s)
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(html: &str, url: &str) -> Song {
        parse_song(html, url, &SelectorOverrides::default()).unwrap()
    }

    #[test]
    fn extracts_with_br_and_filters_junk() {
        let html = r#"<html><head>
            <meta property="og:title" content="Test Song">
            </head><body>
            <span class="artist">Test Artist</span>
            <div class="lyrics">
              first line<br>
              second   line<br><br>
              third line with Embed inside<br>
              更多更詳盡歌詞 在 ※ Mojim.com<br>
              [00:12.34]timed line<br>
              Embed
              <script>var x = 1;</script>
            </div></body></html>"#;
        let song = parse(html, "https://example.com/s/1");
        assert_eq!(song.title, "Test Song");
        assert_eq!(song.artist, "Test Artist");
        assert_eq!(song.lyrics, "first line\nsecond line\n\nthird line with Embed inside");
    }

    #[test]
    fn preserves_newlines_in_preformatted_text() {
        let html = "<div id=\"lyrics\">line one\nline two\n\n\n\nline three</div><h1>T</h1>";
        assert_eq!(parse(html, "https://x.y/").lyrics, "line one\nline two\n\nline three");
    }

    #[test]
    fn genius_title_tag_and_containers() {
        let html = r#"<html><head><title>Some Band – A Tune Lyrics | Genius Lyrics</title></head><body>
            <div data-lyrics-container="true"><div data-exclude-from-selection="true">3 Contributors</div>alpha beta gamma<br>delta</div>
            <div data-lyrics-container="true">epsilon zeta eta</div></body></html>"#;
        let song = parse(html, "https://genius.com/x");
        assert_eq!(song.title, "A Tune");
        assert_eq!(song.artist, "Some Band");
        assert_eq!(song.lyrics, "alpha beta gamma\ndelta\n\nepsilon zeta eta");
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let depth = 50_000;
        let html = format!(
            "<div class=\"lyrics\">{}some words long enough here{}</div>",
            "<span>".repeat(depth),
            "</span>".repeat(depth)
        );
        assert!(parse_song(&html, "https://x.y/", &SelectorOverrides::default()).is_ok());
    }

    #[test]
    fn host_and_profile_matching() {
        assert_eq!(host_of("https://user@www.Genius.com:443/a?b"), "www.genius.com");
        assert_eq!(host_of("http://mojim.com"), "mojim.com");
        // 不應把 evilgenius.com 當作 genius.com
        let html = "<title>A – B Lyrics | Genius Lyrics</title><div class=\"lyrics\">enough words to be lyrics</div>";
        assert_ne!(parse(html, "https://evilgenius.com/").title, "B");
    }

    #[test]
    fn charset_detection() {
        assert_eq!(charset_from_content_type("text/html; charset=\"Big5\""), Some("Big5"));
        assert_eq!(sniff_meta_charset(b"<meta charset='big5'>").as_deref(), Some("big5"));
        assert_eq!(sniff_meta_charset(b"<html>"), None);
    }

    #[test]
    fn validates_inputs() {
        assert!(validate_url("https://a.b/c").is_ok());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("example.com").is_err());
        assert!(SelectorOverrides::new(Some("div[".into()), None, None).is_err());
        assert!(SelectorOverrides::new(Some("  ".into()), None, None).unwrap().title.is_none());
    }

    #[test]
    fn kkbox_structure_and_credits() {
        // 依 KKBOX 實際結構：div.lyrics > p（第一段為製作資訊），段落內以 <br> 換行
        let html = r#"<main><h1>Sample Title</h1><a href="/tw/tc/artist/abc"><img>Sample Singer</a>
            <div class="track-content"><div class="lyrics">
              <p>作詞：Someone　　作曲：Another</p><br>
              <p><br>line one here<br>line two here<br></p>
              <p>line three here<br>作曲：not a credit mid-song</p>
            </div></div></main>"#;
        let song = parse(html, "https://www.kkbox.com/tw/tc/song/xyz");
        assert_eq!(song.title, "Sample Title");
        assert_eq!(song.artist, "Sample Singer");
        assert_eq!(
            song.lyrics,
            "line one here\nline two here\n\nline three here\n作曲：not a credit mid-song"
        );
    }

    #[test]
    fn fragment_and_manual_cleaning() {
        let frag = r#"<div class="lyrics"><p>Composer: X</p><p>alpha<br>beta</p></div>"#;
        assert_eq!(lyrics_from_fragment(frag), "alpha\nbeta");
        assert_eq!(clean_lyrics("  a \r\n\r\n\r\n b  "), "a\n\nb");
        assert_eq!(clean_title("Song - KKBOX"), "Song");
    }

    #[test]
    fn asianpopweekly_titles() {
        let t = asianpopweekly_title("[Lyric Translation] Karencici - Hard to Say 愛你但說不出口 - Asian Pop Weekly");
        assert_eq!(t, Some(("愛你但說不出口 (Hard to Say)".into(), "Karencici".into())));
        let t = asianpopweekly_title("[Lyric Translation] A – 小船 Little Boat").unwrap();
        assert_eq!(t.0, "小船 (Little Boat)");
        assert_eq!(asianpopweekly_title("Six Artists - We Love"), None);

        let html = r#"<title>[Lyric Translation] K - Hard to Say 愛你 - Asian Pop Weekly</title>
            <div class="post-content"><p>Lyrics: K / V</p><p>Composer: K</p><p></p>
            <p>週末 我們去海邊 Let's go to the beach<br>帶上小說 Bring a novel</p></div>"#;
        let song = parse(html, "https://asianpopweekly.com/translations/x/");
        assert_eq!((song.title.as_str(), song.artist.as_str()), ("愛你 (Hard to Say)", "K"));
        assert_eq!(song.lyrics, "週末 我們去海邊 Let's go to the beach\n帶上小說 Bring a novel");
    }

    #[test]
    fn lrc_detection() {
        assert!(is_lrc_tag("[00:12.34]x"));
        assert!(is_lrc_tag("[ti:name]"));
        assert!(!is_lrc_tag("[Chorus]"));
        assert!(!is_lrc_tag("[Verse 1: Someone]"));
    }
}
