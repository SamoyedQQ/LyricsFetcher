//! 依歌名搜尋歌詞網與 YouTube 伴奏。
//!
//! 各來源並行查詢、互不影響：某個網站失敗只回報警告，其餘結果照常顯示。

use std::collections::HashSet;

use reqwest::Url;
use scraper::Html;
use serde::Serialize;
use serde_json::Value;

use crate::crawler;
use crate::genius;
use crate::zh;
use crate::error::{AppError, Result};

/// 每個來源最多列出幾筆
const PER_SITE: usize = 6;
const MAX_VIDEOS: usize = 10;
/// 搜尋結果頁上限（YouTube 約 1.5 MiB）
const MAX_RESPONSE_BYTES: usize = 6 * 1024 * 1024;
const MAX_QUERY_CHARS: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct LyricsHit {
    pub site: &'static str,
    pub title: String,
    pub artist: String,
    pub url: String,
    /// 來源附英譯（中英對照）
    pub translated: bool,
    /// 與查詢的相符程度：0 歌名完全相符、1 簡轉繁後相符、2 部分相符、3 其他
    pub relevance: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct VideoHit {
    pub title: String,
    pub channel: String,
    pub duration: String,
    pub url: String,
}

#[derive(Debug, Default, Serialize)]
pub struct LyricsResults {
    pub hits: Vec<LyricsHit>,
    /// 失敗的來源，例如「Genius：網路錯誤…」
    pub warnings: Vec<String>,
    /// KKBOX 要求瀏覽器驗證（AWS WAF）：GUI 改以內嵌瀏覽器搜尋，見 `kkbox_hits`
    #[serde(rename = "kkboxBlocked")]
    pub kkbox_blocked: bool,
}

fn normalize_query(q: &str) -> Result<String> {
    let q: String = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if q.is_empty() {
        return Err(AppError::Usage("請輸入歌名".into()));
    }
    Ok(q.chars().take(MAX_QUERY_CHARS).collect())
}

/// 以 URL 建構器組網址，查詢字串一律經過編碼
fn url_with(base: &str, params: &[(&str, &str)]) -> Url {
    Url::parse_with_params(base, params).expect("內建網址皆為合法常數")
}

pub(crate) async fn get_text(client: &reqwest::Client, url: Url) -> Result<String> {
    let mut resp = client.get(url.clone()).send().await?;
    let status = resp.status();
    // AWS WAF 的驗證回應是 202 + 空內容，不能當成功
    if !status.is_success() || resp.headers().contains_key("x-amzn-waf-action") {
        return Err(AppError::Http {
            status: status.as_u16(),
            url: url.to_string(),
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(AppError::Parse("搜尋結果過大".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

pub(crate) async fn get_json(client: &reqwest::Client, url: Url) -> Result<Value> {
    serde_json::from_str(&get_text(client, url).await?).map_err(|e| AppError::Parse(format!("回應不是 JSON：{e}")))
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    path.iter().fold(v, |v, k| &v[k]).as_str().unwrap_or_default()
}

/// 只保留 http(s) 網址，避免把奇怪的 scheme 交給前端
fn http_url(s: &str) -> Option<String> {
    crawler::validate_url(s).ok().map(|()| s.to_string())
}

/// 同時查詢所有歌詞網。附英譯（中英對照）的結果排在前面，其餘依來源排序
/// （KKBOX → Asianpopweekly → Genius → LyricsTranslate）。
pub async fn search_lyrics(client: &reqwest::Client, query: &str) -> Result<LyricsResults> {
    let q = normalize_query(query)?;
    // LyricsTranslate 需要瀏覽器，由 GUI 另外以內嵌瀏覽器搜尋（見 `lyricstranslate_hits`）
    let (kkbox, apw, genius) = tokio::join!(kkbox(client, &q), asianpopweekly(client, &q), genius(client, &q));
    let mut out = LyricsResults::default();
    for (site, res) in [("KKBOX", kkbox), ("Asianpopweekly", apw), ("Genius", genius)] {
        match res {
            Ok(hits) => out.hits.extend(hits),
            Err(AppError::Http { status: 202, .. }) if site == "KKBOX" => out.kkbox_blocked = true,
            Err(e) => out.warnings.push(format!("{site}：{e}")),
        }
    }
    // 歌名相符的最優先，同一級中有中英對照的在前；穩定排序，其餘維持來源順序
    for h in &mut out.hits {
        h.relevance = relevance(&q, &h.title, &h.artist);
    }
    out.hits.sort_by_key(|h| (h.relevance, !h.translated));
    Ok(out)
}

/// 比對用：轉小寫、只保留文字與數字（去掉空白、標點、括號）
pub(crate) fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// 去掉結尾的括號說明，例如「愛你但說不出口 (Hard to say)」→「愛你但說不出口」
pub(crate) fn core_title(t: &str) -> &str {
    let mut t = t.trim();
    while let Some(open) = t.rfind(['(', '（'])
        && open > 0
        && t.ends_with([')', '）'])
    {
        t = t[..open].trim_end();
    }
    t
}

/// 歌名（或括號內的別名）與查詢完全相符；查詢可額外包含歌手名，例如「晴天 周杰倫」
fn is_exact(q: &str, title: &str, artist: &str) -> bool {
    let full = norm(title);
    let core = norm(core_title(title));
    // 括號內的英文歌名也算，例如搜尋「Sunny Day」
    let alt = norm(title.trim().get(core_title(title).len()..).unwrap_or_default());
    let artist = norm(artist);
    [&core, &full, &alt].into_iter().filter(|t| !t.is_empty()).any(|t| {
        // 去掉歌名後剩下的部分是歌手名
        *t == q || q.strip_prefix(t.as_str()).or_else(|| q.strip_suffix(t.as_str())).is_some_and(|rest| artist.contains(rest))
    })
}

/// 0：歌名完全相符；1：簡轉繁後完全相符；2：歌名與查詢互相包含（簡繁皆可）；3：其他
fn relevance(query: &str, title: &str, artist: &str) -> u8 {
    let q = norm(query);
    if q.is_empty() || norm(core_title(title)).is_empty() {
        return 3;
    }
    if is_exact(&q, title, artist) {
        return 0;
    }
    // 兩邊都轉成繁體再比一次（KKBOX、Genius 常有簡體版本）
    let (tq, tt, ta) = (norm(&zh::to_traditional(query)), zh::to_traditional(title), zh::to_traditional(artist));
    if is_exact(&tq, &tt, &ta) {
        return 1;
    }
    let core = norm(core_title(&tt));
    if core.contains(&tq) || tq.contains(&core) {
        2
    } else {
        3
    }
}

async fn kkbox(client: &reqwest::Client, q: &str) -> Result<Vec<LyricsHit>> {
    let url = url_with(
        "https://www.kkbox.com/api/search/song",
        &[("q", q), ("terr", "tw"), ("lang", "tc")],
    );
    let v = get_json(client, url).await?;
    Ok(parse_kkbox(&v))
}

/// 內嵌瀏覽器在 KKBOX 網頁內呼叫站內搜尋 API 取得的 JSON（格式同 `kkbox`）
pub fn kkbox_hits(query: &str, v: &Value) -> Vec<LyricsHit> {
    let mut hits = parse_kkbox(v);
    for h in &mut hits {
        h.relevance = relevance(query, &h.title, &h.artist);
    }
    hits
}

fn parse_kkbox(v: &Value) -> Vec<LyricsHit> {
    let empty = Vec::new();
    v["data"]["result"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        // 沒有歌詞的版本（純音樂、部分翻唱）選了也抓不到
        .filter(|r| r["has_lyrics"].as_bool().unwrap_or(true))
        .filter_map(|r| {
            Some(LyricsHit {
                relevance: 3,
                site: "KKBOX",
                title: str_at(r, &["name"]).trim().to_string(),
                artist: str_at(r, &["album", "artist", "name"]).trim().to_string(),
                url: http_url(str_at(r, &["url"]))?,
                translated: false,
            })
        })
        .take(PER_SITE)
        .collect()
}

async fn asianpopweekly(client: &reqwest::Client, q: &str) -> Result<Vec<LyricsHit>> {
    // WordPress REST API 的站內搜尋
    let url = url_with(
        "https://asianpopweekly.com/wp-json/wp/v2/search",
        &[("search", q), ("per_page", "20"), ("subtype", "post")],
    );
    let v = get_json(client, url).await?;
    Ok(parse_asianpopweekly(&v))
}

fn parse_asianpopweekly(v: &Value) -> Vec<LyricsHit> {
    let empty = Vec::new();
    v.as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|r| {
            // 標題含 HTML 實體（&#8211;），交給 HTML 解析器解碼
            let raw = Html::parse_fragment(str_at(r, &["title"]));
            let title: String = raw.root_element().text().collect();
            // 網站也有新聞、專題，只保留歌詞翻譯
            let (title, artist) = crawler::asianpopweekly_title(&title)?;
            Some(LyricsHit {
                relevance: 3,
                site: "Asianpopweekly",
                title,
                artist,
                url: http_url(str_at(r, &["url"]))?,
                translated: true,
            })
        })
        .take(PER_SITE)
        .collect()
}

async fn genius(client: &reqwest::Client, q: &str) -> Result<Vec<LyricsHit>> {
    let url = url_with("https://genius.com/api/search/song", &[("q", q), ("per_page", "10")]);
    let v = get_json(client, url).await?;
    let mut hits = parse_genius(&v);

    // 逐首查詢是否有英文翻譯（並行），有的才標「中英」；查詢失敗只是不標示
    let mut tasks = tokio::task::JoinSet::new();
    for (i, (id, _)) in hits.iter().enumerate() {
        let (client, id) = (client.clone(), *id);
        tasks.spawn(async move { (i, genius::english_translation(&client, id, false).await) });
    }
    while let Some(done) = tasks.join_next().await {
        if let Ok((i, Ok(genius::Translation::Found(_)))) = done {
            hits[i].1.translated = true;
        }
    }
    Ok(hits.into_iter().map(|(_, hit)| hit).collect())
}

/// 回傳 (歌曲 ID, 結果)。Genius 自己的翻譯、羅馬拼音頁不列出：選原文頁時會自動合併英譯。
fn parse_genius(v: &Value) -> Vec<(u64, LyricsHit)> {
    let empty = Vec::new();
    v["response"]["sections"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .flat_map(|s| s["hits"].as_array().unwrap_or(&empty))
        .map(|h| &h["result"])
        .filter_map(|r| {
            let artist = str_at(r, &["artist_names"]).trim().to_string();
            if artist.starts_with("Genius ") {
                return None;
            }
            let hit = LyricsHit {
                relevance: 3,
                site: "Genius",
                title: str_at(r, &["title"]).trim().to_string(),
                translated: false,
                artist,
                url: http_url(str_at(r, &["url"]))?,
            };
            Some((r["id"].as_u64()?, hit))
        })
        .take(PER_SITE)
        .collect()
}

/// LyricsTranslate 的搜尋結果：(連結, 標題) 來自內嵌瀏覽器載入的 DuckDuckGo `site:` 搜尋頁。
/// LyricsTranslate（Cloudflare）與 DuckDuckGo 都會擋程式直接發出的請求，所以搜尋與擷取都交給瀏覽器。
pub fn lyricstranslate_hits(query: &str, links: &[(String, String)]) -> Vec<LyricsHit> {
    let mut seen = HashSet::new();
    let mut hits: Vec<LyricsHit> = links
        .iter()
        .filter_map(|(href, text)| {
            let url = ddg_target(href)?;
            let (title, artist, translated) = lyricstranslate_title(text)?;
            seen.insert(url.clone()).then_some(LyricsHit {
                relevance: 3,
                site: "LyricsTranslate",
                title,
                artist,
                url,
                translated,
            })
        })
        .take(PER_SITE)
        .collect();
    for h in &mut hits {
        h.relevance = relevance(query, &h.title, &h.artist);
    }
    hits
}

/// DuckDuckGo 的結果連結是 `//duckduckgo.com/l/?uddg=<目標網址>`；只接受 lyricstranslate.com 的 https 歌曲頁
fn ddg_target(href: &str) -> Option<String> {
    let abs = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    let u = Url::parse(&abs).ok()?;
    let target = if u.host_str()?.ends_with("duckduckgo.com") {
        u.query_pairs().find(|(k, _)| k == "uddg")?.1.into_owned()
    } else {
        abs
    };
    let t = Url::parse(&target).ok()?;
    let host = t.host_str()?;
    let ok = t.scheme() == "https"
        && (host == "lyricstranslate.com" || host.ends_with(".lyricstranslate.com"))
        && t.path().ends_with(".html");
    ok.then(|| t.to_string())
}

/// 「歌手 - 歌名 (English translation #2)」→ (歌名, 歌手, true)；「歌手 - 歌名 lyrics」→ (歌名, 歌手, false)。
/// 其他語言的翻譯、羅馬拼音、歌手頁回傳 None。
pub fn lyricstranslate_title(t: &str) -> Option<(String, String, bool)> {
    let t = t.trim();
    let t = t.strip_suffix(" - Lyrics Translate").unwrap_or(t).trim();
    let (rest, translated) = match t.rfind(" (English translation") {
        Some(i) if t.ends_with(')') => (&t[..i], true),
        Some(_) => return None,
        None => (t.strip_suffix(" lyrics")?, false),
    };
    let (artist, title) = rest.split_once(" - ")?;
    let (title, artist) = (title.trim(), artist.trim());
    (!title.is_empty() && !artist.is_empty()).then(|| (title.to_string(), artist.to_string(), translated))
}

/// 在 YouTube 搜尋（呼叫端決定關鍵字，例如「歌名 伴奏」）
pub async fn search_videos(client: &reqwest::Client, query: &str) -> Result<Vec<VideoHit>> {
    let q = normalize_query(query)?;
    let url = url_with(
        "https://www.youtube.com/results",
        &[("search_query", &q), ("hl", "zh-TW"), ("gl", "TW")],
    );
    let html = get_text(client, url).await?;
    parse_youtube(&html)
}

/// 從搜尋頁內嵌的 `ytInitialData` JSON 取出影片
fn parse_youtube(html: &str) -> Result<Vec<VideoHit>> {
    const MARKER: &str = "var ytInitialData = ";
    let start = html
        .find(MARKER)
        .ok_or_else(|| AppError::Parse("YouTube 搜尋頁格式已變更，找不到結果資料".into()))?
        + MARKER.len();
    // 只解析第一個 JSON 值，後面的 `;</script>` 不必另外尋找
    let data: Value = serde_json::Deserializer::from_str(&html[start..])
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Parse("YouTube 結果資料是空的".into()))?
        .map_err(|e| AppError::Parse(format!("YouTube 結果資料無法解析：{e}")))?;

    let mut out = Vec::new();
    collect_videos(&data, &mut out);
    Ok(out)
}

/// 結果巢狀很深且結構常變，直接遞迴找所有 videoRenderer。
/// 以明確的堆疊取代遞迴，避免異常深的 JSON 造成堆疊溢位。
fn collect_videos(root: &Value, out: &mut Vec<VideoHit>) {
    let mut stack = vec![root];
    while let Some(v) = stack.pop() {
        if out.len() >= MAX_VIDEOS {
            return;
        }
        match v {
            Value::Object(map) => {
                if let Some(r) = map.get("videoRenderer") {
                    let id = str_at(r, &["videoId"]);
                    // videoId 只會是英數、-、_；其他字元一律不採用
                    if !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                        out.push(VideoHit {
                            title: run_text(&r["title"]),
                            channel: run_text(&r["ownerText"]),
                            duration: str_at(r, &["lengthText", "simpleText"]).to_string(),
                            url: format!("https://www.youtube.com/watch?v={id}"),
                        });
                    }
                    continue;
                }
                // 反向推入，維持搜尋結果原本的順序
                stack.extend(map.values().rev());
            }
            Value::Array(items) => stack.extend(items.iter().rev()),
            _ => {}
        }
    }
}

fn run_text(v: &Value) -> String {
    match v["runs"].as_array() {
        Some(runs) => runs.iter().map(|r| str_at(r, &["text"])).collect(),
        None => str_at(v, &["simpleText"]).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ranks_exact_titles_first() {
        assert_eq!(relevance("當你", "當你", "林俊傑 (JJ Lin)"), 0);
        assert_eq!(relevance("晴天 周杰倫", "晴天 (Sunny Day)", "周杰倫 (Jay Chou)"), 0);
        assert_eq!(relevance("sunny day", "晴天 (Sunny Day)", "x"), 0);
        assert_eq!(relevance("簡體中文", "简体中文", "x"), 1);
        assert_eq!(relevance("簡體中文 歌手", "简体中文 (Test)", "歌手"), 1);
        assert_eq!(relevance("當你", "當你孤單你會想起誰", "x"), 2);
        assert_eq!(relevance("當你", "当你的心", "x"), 2);
        assert_eq!(relevance("當你", "藍蓮花 (Blue Lotus)", "許巍"), 3);
        assert_eq!(relevance("晴天", "晴天 周杰倫", "Someone"), 2);
    }

    #[test]
    fn parses_kkbox() {
        let v = json!({ "data": { "result": [
            { "name": "愛你但說不出口", "url": "https://www.kkbox.com/tw/tc/song/a", "has_lyrics": true,
              "album": { "artist": { "name": "Karencici" } } },
            { "name": "純音樂", "url": "https://www.kkbox.com/tw/tc/song/b", "has_lyrics": false },
            { "name": "壞網址", "url": "javascript:alert(1)", "has_lyrics": true }
        ] } });
        let hits = parse_kkbox(&v);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].artist, "Karencici");
    }

    #[test]
    fn parses_asianpopweekly() {
        let v = json!([
            { "title": "[Lyric Translation] Karencici &#8211; Hard to Say \u{611b}\u{4f60}", "url": "https://asianpopweekly.com/t/a/" },
            { "title": "Princesses of Asian Pop &#8211; Six Artists", "url": "https://asianpopweekly.com/x/" }
        ]);
        let hits = parse_asianpopweekly(&v);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "愛你 (Hard to Say)");
        assert_eq!(hits[0].artist, "Karencici");
        assert!(hits[0].translated);
    }

    #[test]
    fn parses_lyricstranslate_results() {
        let a = |href: &str, text: &str| (href.to_string(), text.to_string());
        let links = [
            a("//duckduckgo.com/l/?uddg=https%3A%2F%2Flyricstranslate.com%2Fen%2Fsong-a.html&rut=x", "Singer - 歌名 (Ge Ming) (English translation)"),
            a("https://duckduckgo.com/l/?uddg=https%3A%2F%2Flyricstranslate.com%2Fen%2Fsong-b.html&rut=y", "Singer - 歌名 (Ge Ming) (English translation #2)"),
            a("//duckduckgo.com/l/?uddg=https%3A%2F%2Flyricstranslate.com%2Fen%2Fsong-lyrics.html", "Singer - 歌名 (Ge Ming) lyrics"),
            a("//duckduckgo.com/l/?uddg=https%3A%2F%2Flyricstranslate.com%2Fen%2Fsinger-lyrics.html", "Singer (歌手) lyrics with translations"),
            a("//duckduckgo.com/l/?uddg=https%3A%2F%2Flyricstranslate.com%2Fen%2Fsong-r.html", "Singer - 歌名 (Ge Ming) (Romanization) - Lyrics Translate"),
            a("//duckduckgo.com/l/?uddg=https%3A%2F%2Fevil.example%2Fx.html", "Singer - 歌名 (English translation)"),
        ];
        let hits = lyricstranslate_hits("歌名", &links);
        assert!(hits.iter().all(|h| h.relevance == 0));
        let got: Vec<(&str, bool)> = hits.iter().map(|h| (h.url.as_str(), h.translated)).collect();
        assert_eq!(
            got,
            [
                ("https://lyricstranslate.com/en/song-a.html", true),
                ("https://lyricstranslate.com/en/song-b.html", true),
                ("https://lyricstranslate.com/en/song-lyrics.html", false),
            ]
        );
        assert_eq!((hits[0].title.as_str(), hits[0].artist.as_str()), ("歌名 (Ge Ming)", "Singer"));
    }

    #[test]
    fn parses_youtube() {
        let html = r#"<script>var ytInitialData = {"contents":{"a":[
            {"videoRenderer":{"videoId":"abc_-1","title":{"runs":[{"text":"歌 "},{"text":"伴奏"}]},
              "ownerText":{"runs":[{"text":"頻道"}]},"lengthText":{"simpleText":"3:00"}}},
            {"videoRenderer":{"videoId":"bad\"id","title":{"runs":[{"text":"x"}]}}},
            {"videoRenderer":{"videoId":"def","title":{"runs":[{"text":"第二"}]}}}
        ]}};</script>"#;
        let v = parse_youtube(html).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].title, "歌 伴奏");
        assert_eq!(v[0].channel, "頻道");
        assert_eq!(v[0].url, "https://www.youtube.com/watch?v=abc_-1");
        assert_eq!(v[1].title, "第二");
        assert!(parse_youtube("<html></html>").is_err());
    }
}
