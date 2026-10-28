//! Notion API：讀取資料庫結構、依欄位型別建立頁面、分批寫入正文。
//!
//! 欄位對應不寫死型別——先 GET 資料庫 schema，再依實際型別
//! （title / rich_text / select / multi_select / url）組出正確的 JSON。
//! 歌手以外的 Select / Multi-select 欄位視為「標籤」，選項由使用者挑選；
//! 歌手、歌詞以外的 Text 欄位（伴奏、備註…）由使用者填寫，可附連結。

use std::collections::HashMap;
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, HeaderValue};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::bilingual::{self, Line};
use crate::crawler::{self, Song};
use crate::error::{AppError, Result};

const API_BASE: &str = "https://api.notion.com/v1";
/// 2025-09-03 之後的版本改用 data source 模型，`parent.database_id` 寫法需調整，故固定此版
const DEFAULT_VERSION: &str = "2022-06-28";
/// 單一 rich text 物件的 content 上限（Notion 以 UTF-16 code unit 計算）
const TEXT_LIMIT: usize = 2000;
/// 單一 rich text 陣列 / 單次 children 請求的項目上限
const MAX_ITEMS: usize = 100;
const MAX_RETRIES: u32 = 3;
/// 讀取既有填寫值時最多掃描的頁數（每頁 100 筆）
const SUGGEST_PAGES: usize = 3;
const SUGGEST_MAX: usize = 8;

const ARTIST_NAMES: &[&str] = &["Artist", "歌手", "Singer", "演唱者", "藝人", "Artists"];
const LYRICS_NAMES: &[&str] = &["Lyrics", "歌詞", "Lyric"];
const URL_NAMES: &[&str] = &["URL", "Link", "Source", "來源", "網址", "連結"];

/// 連線設定。欄位名稱為 None 時以常見名稱自動比對。
#[derive(Clone, Default)]
pub struct NotionConfig {
    pub api_key: String,
    /// 可為 32 碼 ID、UUID 或整個資料庫網址
    pub database_id: String,
    pub version: Option<String>,
    pub artist_property: Option<String>,
    pub lyrics_property: Option<String>,
    pub url_property: Option<String>,
}

impl std::fmt::Debug for NotionConfig {
    // 手動實作，避免金鑰出現在 Debug 輸出
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotionConfig")
            .field("api_key", &"<redacted>")
            .field("database_id", &self.database_id)
            .finish_non_exhaustive()
    }
}

impl NotionConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            api_key: env_nonempty("NOTION_API_KEY")
                .ok_or_else(|| AppError::Config("缺少 NOTION_API_KEY（請參考 .env.example）".into()))?,
            database_id: env_nonempty("DATABASE_ID")
                .ok_or_else(|| AppError::Config("缺少 DATABASE_ID（請參考 .env.example）".into()))?,
            version: env_nonempty("NOTION_VERSION"),
            artist_property: env_nonempty("NOTION_ARTIST_PROPERTY"),
            lyrics_property: env_nonempty("NOTION_LYRICS_PROPERTY"),
            url_property: env_nonempty("NOTION_URL_PROPERTY"),
        })
    }
}

/// 驗證 API 金鑰格式（不含空白或控制字元）
pub fn validate_api_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(AppError::Config("尚未設定 Notion API Key".into()));
    }
    if key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::Config("Notion API Key 含有空白或控制字元".into()));
    }
    Ok(())
}

pub struct Notion {
    client: reqwest::Client,
    auth: HeaderValue,
    version: HeaderValue,
    database_id: String,
    overrides: [Option<String>; 3],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    RichText,
    Select,
    MultiSelect,
}

#[derive(Debug, Clone)]
pub struct Schema {
    title: String,
    artist: Option<(String, Kind)>,
    lyrics: Option<String>,
    url: Option<String>,
    tags: Vec<TagField>,
    /// 歌手、歌詞以外的 Text 欄位，依名稱排序
    texts: Vec<String>,
    /// 解析 schema 時發現、但不影響執行的問題（例如欄位型別不支援）
    pub warnings: Vec<String>,
}

/// 可供挑選的標籤欄位（歌手以外的 Select / Multi-select）
#[derive(Debug, Clone, Serialize)]
pub struct TagField {
    pub name: String,
    pub multi: bool,
    pub options: Vec<TagOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagOption {
    pub name: String,
    /// Notion 的選項顏色名稱（default、gray、blue…）
    #[serde(default)]
    pub color: String,
}

/// 使用者為某個標籤欄位選定的選項
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Tag {
    pub property: String,
    pub values: Vec<String>,
}

/// 使用者為某個 Text 欄位填寫的內容，例如伴奏「和聲伴奏」＋ YouTube 連結
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TextValue {
    pub property: String,
    pub text: String,
    #[serde(default)]
    pub link: Option<String>,
}

/// 建立頁面時的選項
#[derive(Debug, Clone, Default)]
pub struct PageOptions {
    pub tags: Vec<Tag>,
    pub texts: Vec<TextValue>,
    /// 歌詞為「一行原文、一行英譯」格式，正文以 Heading 2 / 橘色粗體寫入
    pub bilingual: bool,
    pub write_body: bool,
}

/// 某個 Text 欄位在資料庫中常用的值（依出現次數排序）
#[derive(Debug, Clone, Serialize)]
pub struct FieldValues {
    pub property: String,
    pub values: Vec<String>,
}

impl Schema {
    /// 給使用者看的欄位對應摘要
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("歌名→{}", self.title)];
        if let Some((name, _)) = &self.artist {
            parts.push(format!("歌手→{name}"));
        }
        if let Some(name) = &self.lyrics {
            parts.push(format!("歌詞→{name}"));
        }
        if let Some(name) = &self.url {
            parts.push(format!("網址→{name}"));
        }
        if !self.tags.is_empty() {
            let names: Vec<&str> = self.tags.iter().map(|t| t.name.as_str()).collect();
            parts.push(format!("標籤→{}", names.join("、")));
        }
        if !self.texts.is_empty() {
            parts.push(format!("文字→{}", self.texts.join("、")));
        }
        parts.join(" · ")
    }

    /// 依名稱排序的標籤欄位，選項順序與 Notion 中相同
    pub fn tags(&self) -> &[TagField] {
        &self.tags
    }

    /// 可填寫的 Text 欄位名稱（歌手、歌詞除外）
    pub fn text_fields(&self) -> &[String] {
        &self.texts
    }
}

pub struct Created {
    pub url: String,
    pub blocks: usize,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct PropDef {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    select: Option<OptionList>,
    #[serde(default)]
    multi_select: Option<OptionList>,
}

#[derive(Deserialize, Default)]
struct OptionList {
    #[serde(default)]
    options: Vec<TagOption>,
}

#[derive(Deserialize)]
struct DatabaseResp {
    properties: Option<HashMap<String, PropDef>>,
}

#[derive(Deserialize, Default)]
struct ErrorResp {
    #[serde(default)]
    code: String,
    #[serde(default)]
    message: String,
}

impl Notion {
    pub fn new(client: reqwest::Client, cfg: &NotionConfig) -> Result<Self> {
        let key = cfg.api_key.trim();
        validate_api_key(key)?;
        let mut auth = HeaderValue::from_str(&format!("Bearer {key}"))
            .map_err(|_| AppError::Config("Notion API Key 含有無效字元".into()))?;
        // 標記為敏感：Debug 輸出時不會印出金鑰
        auth.set_sensitive(true);

        let database_id = normalize_id(&cfg.database_id)
            .ok_or_else(|| AppError::Config(format!("Database ID 格式不正確：{}", cfg.database_id)))?;

        let version = cfg.version.as_deref().unwrap_or(DEFAULT_VERSION);
        let version = HeaderValue::from_str(version)
            .map_err(|_| AppError::Config("NOTION_VERSION 含有無效字元".into()))?;

        let prop = |p: &Option<String>| p.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
        Ok(Self {
            client,
            auth,
            version,
            database_id,
            overrides: [
                prop(&cfg.artist_property),
                prop(&cfg.lyrics_property),
                prop(&cfg.url_property),
            ],
        })
    }

    /// 正規化後的資料庫 ID（可作為快取鍵）
    pub fn database_id(&self) -> &str {
        &self.database_id
    }

    pub async fn fetch_schema(&self) -> Result<Schema> {
        let resp = self
            .call(Method::GET, &format!("/databases/{}", self.database_id), None)
            .await?;
        let db: DatabaseResp = serde_json::from_value(resp).map_err(|e| invalid_json(&e))?;
        let props = db.properties.ok_or_else(|| AppError::Notion {
            status: 200,
            code: "unexpected_response".into(),
            message: "回應中沒有 properties 欄位，請確認 NOTION_VERSION 為 2022-06-28".into(),
        })?;
        resolve_schema(&props, &self.overrides)
    }

    pub async fn create_page(&self, schema: &Schema, song: &Song, opts: &PageOptions) -> Result<Created> {
        let mut warnings = Vec::new();
        let mut properties = build_properties(schema, song, &mut warnings);
        properties.extend(tag_properties(schema, &opts.tags, &mut warnings));
        properties.extend(text_properties(schema, &opts.texts, &mut warnings));
        let blocks = match (opts.write_body, opts.bilingual) {
            (false, _) => Vec::new(),
            (true, true) => bilingual_blocks(&song.lyrics),
            (true, false) => lyrics_blocks(&song.lyrics),
        };
        let total = blocks.len();
        let mut batches = blocks.chunks(MAX_ITEMS);

        let body = json!({
            "parent": { "database_id": self.database_id },
            "properties": properties,
            "children": batches.next().unwrap_or(&[]),
        });
        let page = self.call(Method::POST, "/pages", Some(&body)).await?;

        let page_url = page["url"].as_str().unwrap_or_default().to_string();
        let page_id = page["id"]
            .as_str()
            .and_then(normalize_id)
            .ok_or_else(|| AppError::Notion {
                status: 200,
                code: "unexpected_response".into(),
                message: "建立頁面後未取得有效的頁面 ID".into(),
            })?;

        // 超過 100 個區塊需分批附加；必須依序送出以維持段落順序
        for batch in batches {
            let body = json!({ "children": batch });
            self.call(Method::PATCH, &format!("/blocks/{page_id}/children"), Some(&body))
                .await
                .map_err(|e| match e {
                    AppError::Notion { status, code, message } => AppError::Notion {
                        status,
                        code,
                        message: format!("{message}\n頁面已建立但正文未完整寫入：{page_url}"),
                    },
                    other => other,
                })?;
        }

        Ok(Created {
            url: page_url,
            blocks: total,
            warnings,
        })
    }

    /// 讀取資料庫前幾百筆頁面，統計每個 Text 欄位常用的值（例如伴奏「純伴奏」、備註「原KEY」）
    pub async fn text_suggestions(&self, schema: &Schema) -> Result<Vec<FieldValues>> {
        let mut counts: HashMap<&str, HashMap<String, usize>> = HashMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..SUGGEST_PAGES {
            let mut body = json!({ "page_size": MAX_ITEMS });
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let resp = self
                .call(Method::POST, &format!("/databases/{}/query", self.database_id), Some(&body))
                .await?;
            for page in resp["results"].as_array().into_iter().flatten() {
                for field in &schema.texts {
                    let text: String = page["properties"][field]["rich_text"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|t| t["plain_text"].as_str())
                        .collect();
                    let text = text.trim();
                    if !text.is_empty() && text.chars().count() <= 40 {
                        *counts.entry(field).or_default().entry(text.to_string()).or_default() += 1;
                    }
                }
            }
            cursor = resp["next_cursor"].as_str().map(String::from);
            if resp["has_more"] != json!(true) || cursor.is_none() {
                break;
            }
        }
        Ok(schema
            .texts
            .iter()
            .map(|field| {
                let mut values: Vec<(String, usize)> =
                    counts.remove(field.as_str()).unwrap_or_default().into_iter().collect();
                values.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                FieldValues {
                    property: field.clone(),
                    values: values.into_iter().take(SUGGEST_MAX).map(|(v, _)| v).collect(),
                }
            })
            .collect())
    }

    /// 送出請求並解析 JSON。
    ///
    /// 429 一律重試（Notion 保證未處理）；5xx 只對 GET 重試，
    /// 避免 POST 實際已成功卻因逾時重送而建立重複頁面。
    async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{API_BASE}{path}");
        let mut attempt = 0;
        loop {
            let mut req = self
                .client
                .request(method.clone(), &url)
                .header(AUTHORIZATION, self.auth.clone())
                .header("Notion-Version", self.version.clone());
            if let Some(b) = body {
                req = req.json(b);
            }
            let resp = req.send().await?;
            let status = resp.status();

            let retryable = status == StatusCode::TOO_MANY_REQUESTS
                || (method == Method::GET && status.is_server_error());
            if retryable && attempt < MAX_RETRIES {
                let wait = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1 << attempt)
                    .clamp(1, 30);
                tokio::time::sleep(Duration::from_secs(wait)).await;
                attempt += 1;
                continue;
            }

            let bytes = resp.bytes().await?;
            if status.is_success() {
                return serde_json::from_slice(&bytes).map_err(|e| invalid_json(&e));
            }
            let err: ErrorResp = serde_json::from_slice(&bytes).unwrap_or_else(|_| ErrorResp {
                code: "unknown".into(),
                message: String::from_utf8_lossy(&bytes).chars().take(300).collect(),
            });
            return Err(AppError::Notion {
                status: status.as_u16(),
                code: err.code,
                message: err.message,
            });
        }
    }
}

fn invalid_json(e: &serde_json::Error) -> AppError {
    AppError::Notion {
        status: 200,
        code: "invalid_json".into(),
        message: format!("無法解析 Notion 回應：{e}"),
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// 接受純 ID、帶連字號的 UUID 或整個 Notion 網址，回傳 32 字元小寫十六進位 ID。
/// 只輸出十六進位字元，可安全放進 URL 路徑。
pub fn normalize_id(raw: &str) -> Option<String> {
    let path = raw.split(['?', '#']).next()?;
    let compact: String = path.chars().filter(|&c| c != '-').collect();
    compact
        .split(|c: char| !c.is_ascii_hexdigit())
        .filter(|s| s.len() >= 32)
        .next_back()
        .map(|s| s[s.len() - 32..].to_ascii_lowercase())
}

// ─── 欄位對應 ───────────────────────────────────────────────────────────

/// `overrides` 依序為 [歌手, 歌詞, 網址] 的使用者指定欄位名稱
fn resolve_schema(props: &HashMap<String, PropDef>, overrides: &[Option<String>; 3]) -> Result<Schema> {
    let available = || {
        let mut names: Vec<_> = props.iter().map(|(n, d)| format!("{n}({})", d.kind)).collect();
        names.sort();
        names.join(", ")
    };

    let title = props
        .iter()
        .find(|(_, d)| d.kind == "title")
        .map(|(n, _)| n.clone())
        .ok_or_else(|| AppError::Config("資料庫沒有標題欄位".into()))?;

    // 使用者指定欄位名稱時，找不到就報錯；未指定則用常見名稱自動比對，找不到就略過
    let find = |custom: &Option<String>, label: &str, candidates: &[&str]| -> Result<Option<(String, String)>> {
        if let Some(name) = custom {
            return props
                .get(name)
                .map(|d| Some((name.clone(), d.kind.clone())))
                .ok_or_else(|| {
                    AppError::Config(format!("指定的{label}欄位「{name}」不存在。現有欄位：{}", available()))
                });
        }
        Ok(candidates.iter().find_map(|cand| {
            props
                .iter()
                .find(|(n, _)| n.trim().eq_ignore_ascii_case(cand))
                .map(|(n, d)| (n.clone(), d.kind.clone()))
        }))
    };

    let mut warnings = Vec::new();
    let mut unsupported = |name: &str, kind: &str| {
        warnings.push(format!("欄位「{name}」型別為 {kind}，不支援，已略過"));
    };
    let [artist_override, lyrics_override, url_override] = overrides;

    let artist = match find(artist_override, "歌手", ARTIST_NAMES)? {
        Some((n, k)) => match k.as_str() {
            "rich_text" => Some((n, Kind::RichText)),
            "select" => Some((n, Kind::Select)),
            "multi_select" => Some((n, Kind::MultiSelect)),
            _ => {
                unsupported(&n, &k);
                None
            }
        },
        None => None,
    };

    let lyrics = match find(lyrics_override, "歌詞", LYRICS_NAMES)? {
        Some((n, k)) if k == "rich_text" => Some(n),
        Some((n, k)) => {
            unsupported(&n, &k);
            None
        }
        None => None,
    };

    let url = match find(url_override, "網址", URL_NAMES)? {
        Some((n, k)) if k == "url" => Some(n),
        Some((n, k)) => {
            unsupported(&n, &k);
            None
        }
        None => None,
    };

    let artist_name = artist.as_ref().map(|(n, _)| n.as_str());
    let mut tags: Vec<TagField> = props
        .iter()
        .filter(|(n, _)| Some(n.as_str()) != artist_name)
        .filter_map(|(n, d)| {
            let (multi, list) = match d.kind.as_str() {
                "select" => (false, d.select.as_ref()),
                "multi_select" => (true, d.multi_select.as_ref()),
                _ => return None,
            };
            Some(TagField {
                name: n.clone(),
                multi,
                options: list.map(|l| l.options.clone()).unwrap_or_default(),
            })
        })
        .collect();
    tags.sort_by(|a, b| a.name.cmp(&b.name));

    let lyrics_name = lyrics.as_deref();
    let mut texts: Vec<String> = props
        .iter()
        .filter(|(n, d)| {
            d.kind == "rich_text" && Some(n.as_str()) != artist_name && Some(n.as_str()) != lyrics_name
        })
        .map(|(n, _)| n.clone())
        .collect();
    texts.sort();

    Ok(Schema {
        title,
        artist,
        lyrics,
        url,
        tags,
        texts,
        warnings,
    })
}

/// 只接受資料庫中已存在的欄位與選項，避免打錯字時在 Notion 自動新增選項
fn tag_properties(schema: &Schema, tags: &[Tag], warnings: &mut Vec<String>) -> Map<String, Value> {
    let mut props = Map::new();
    for tag in tags {
        let Some(field) = schema.tags.iter().find(|f| f.name == tag.property) else {
            warnings.push(format!("標籤欄位「{}」不存在，已略過", tag.property));
            continue;
        };
        let mut chosen: Vec<&str> = Vec::new();
        for v in &tag.values {
            if !field.options.iter().any(|o| &o.name == v) {
                warnings.push(format!("「{}」沒有「{v}」這個選項，已略過", field.name));
            } else if !chosen.contains(&v.as_str()) {
                chosen.push(v);
            }
        }
        if chosen.is_empty() {
            continue;
        }
        let value = if field.multi {
            let opts: Vec<Value> = chosen.iter().map(|n| json!({ "name": n })).collect();
            json!({ "multi_select": opts })
        } else {
            if chosen.len() > 1 {
                warnings.push(format!("「{}」只能單選，已使用「{}」", field.name, chosen[0]));
            }
            json!({ "select": { "name": chosen[0] } })
        };
        props.insert(field.name.clone(), value);
    }
    props
}

/// 只寫入資料庫中存在的 Text 欄位；連結必須是 http(s)
fn text_properties(schema: &Schema, texts: &[TextValue], warnings: &mut Vec<String>) -> Map<String, Value> {
    let mut props = Map::new();
    for t in texts {
        if !schema.texts.contains(&t.property) {
            warnings.push(format!("文字欄位「{}」不存在，已略過", t.property));
            continue;
        }
        let link = t.link.as_deref().map(str::trim).filter(|l| !l.is_empty());
        let link = match link {
            Some(l) if crawler::validate_url(l).is_ok() && l.len() <= TEXT_LIMIT => Some(l),
            Some(l) => {
                warnings.push(format!("「{}」的連結無效，已略過：{l}", t.property));
                None
            }
            None => None,
        };
        let text = t.text.trim();
        // 只有連結時，以網址本身當作顯示文字
        let text = if text.is_empty() { link.unwrap_or_default() } else { text };
        if text.is_empty() {
            continue;
        }
        let items: Vec<Value> = text_chunks(text)
            .into_iter()
            .take(MAX_ITEMS)
            .map(|c| match link {
                Some(url) => json!({ "type": "text", "text": { "content": c, "link": { "url": url } } }),
                None => json!({ "type": "text", "text": { "content": c } }),
            })
            .collect();
        props.insert(t.property.clone(), json!({ "rich_text": items }));
    }
    props
}

fn build_properties(schema: &Schema, song: &Song, warnings: &mut Vec<String>) -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(schema.title.clone(), json!({ "title": rich_text(&song.title, 1) }));

    if let Some((name, kind)) = &schema.artist
        && !song.artist.is_empty()
    {
        let value = match kind {
            Kind::RichText => json!({ "rich_text": rich_text(&song.artist, MAX_ITEMS) }),
            Kind::Select => json!({ "select": { "name": option_name(&song.artist) } }),
            Kind::MultiSelect => {
                let opts: Vec<Value> = split_artists(&song.artist)
                    .iter()
                    .map(|a| json!({ "name": option_name(a) }))
                    .collect();
                json!({ "multi_select": opts })
            }
        };
        props.insert(name.clone(), value);
    }

    if let Some(name) = &schema.lyrics {
        let items = rich_text(&song.lyrics, MAX_ITEMS);
        if text_chunks(&song.lyrics).len() > MAX_ITEMS {
            warnings.push(format!(
                "歌詞超過屬性上限（{} 字），「{name}」欄位已截斷；正文區塊仍為完整內容",
                MAX_ITEMS * TEXT_LIMIT
            ));
        }
        props.insert(name.clone(), json!({ "rich_text": items }));
    }

    // 手動輸入時可能沒有來源網址
    if let Some(name) = &schema.url
        && !song.source_url.is_empty()
    {
        props.insert(name.clone(), json!({ "url": song.source_url }));
    }
    props
}

/// Select 選項名稱不可含逗號，且上限 100 字元
fn option_name(s: &str) -> String {
    s.replace(',', "，").chars().take(100).collect()
}

/// 把 "A, B & C feat. D" 拆成多個歌手（multi_select 用）
fn split_artists(s: &str) -> Vec<String> {
    let mut normalized = s.to_string();
    for sep in [" feat. ", " feat ", " ft. ", " Feat. ", " FEAT. ", " x ", " X ", " & ", "&", "＆", "、", "，", "/", ";"] {
        normalized = normalized.replace(sep, ",");
    }
    let mut out: Vec<String> = Vec::new();
    for part in normalized.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(part)) {
            out.push(part.to_string());
        }
    }
    out
}

fn rich_text(text: &str, max_items: usize) -> Vec<Value> {
    text_chunks(text)
        .into_iter()
        .take(max_items)
        .map(|c| json!({ "type": "text", "text": { "content": c } }))
        .collect()
}

/// 每段一個 paragraph 區塊；超長段落拆成多個區塊，確保不遺失內容。
fn lyrics_blocks(lyrics: &str) -> Vec<Value> {
    lyrics
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .flat_map(|stanza| {
            text_chunks(stanza)
                .chunks(MAX_ITEMS)
                .map(|group| {
                    let rt: Vec<Value> = group
                        .iter()
                        .map(|c| json!({ "type": "text", "text": { "content": c } }))
                        .collect();
                    json!({ "object": "block", "type": "paragraph", "paragraph": { "rich_text": rt } })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// 中英對照：原文 Heading 2、英譯橘色粗體、段落標記粗體 `<Verse 1>`、段落間空一行（與資料庫既有頁面一致）
fn bilingual_blocks(lyrics: &str) -> Vec<Value> {
    let block = |kind: &str, text: &str, annotations: &Value| {
        let rt: Vec<Value> = text_chunks(text)
            .into_iter()
            .take(MAX_ITEMS)
            .map(|c| json!({ "type": "text", "text": { "content": c }, "annotations": annotations }))
            .collect();
        json!({ "object": "block", "type": kind, kind: { "rich_text": rt } })
    };
    let plain = json!({});
    let bold = json!({ "bold": true });
    let orange = json!({ "bold": true, "color": "orange" });
    bilingual::classify(lyrics)
        .into_iter()
        .map(|line| match line {
            Line::Blank => block("paragraph", "", &plain),
            Line::Section(s) => block("paragraph", &format!("<{s}>"), &bold),
            Line::Lyric(s) => block("heading_2", s, &plain),
            Line::Translation(s) => block("paragraph", s, &orange),
        })
        .collect()
}

/// 依 UTF-16 長度切成 ≤ 2000 的片段，盡量在換行處切開。
fn text_chunks(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut units = 0;
    let mut last_nl = None;
    for (i, c) in s.char_indices() {
        let u = c.len_utf16();
        if units + u > TEXT_LIMIT {
            let cut = match last_nl {
                Some(n) if n > start => n,
                _ => i,
            };
            out.push(&s[start..cut]);
            units = s[cut..i].encode_utf16().count();
            start = cut;
            last_nl = None;
        }
        units += u;
        if c == '\n' {
            last_nl = Some(i + 1);
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_ids() {
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(normalize_id(id).as_deref(), Some(id));
        assert_eq!(
            normalize_id("01234567-89ab-cdef-0123-456789abcdef").as_deref(),
            Some(id)
        );
        assert_eq!(
            normalize_id(&format!("https://www.notion.so/My-Cafe-{id}?v=fedcba9876543210fedcba9876543210")).as_deref(),
            Some(id)
        );
        assert_eq!(normalize_id("../../users"), None);
    }

    #[test]
    fn chunks_respect_limit_and_utf16() {
        let s = "😀".repeat(1500); // 每個 2 個 UTF-16 code unit
        let chunks = text_chunks(&s);
        assert!(chunks.iter().all(|c| c.encode_utf16().count() <= TEXT_LIMIT));
        assert_eq!(chunks.concat(), s);

        let lines = "歌詞行\n".repeat(1000);
        let chunks = text_chunks(&lines);
        assert!(chunks.iter().all(|c| c.encode_utf16().count() <= TEXT_LIMIT));
        assert!(chunks[..chunks.len() - 1].iter().all(|c| c.ends_with('\n')));
        assert_eq!(chunks.concat(), lines);
    }

    #[test]
    fn splits_artists() {
        assert_eq!(split_artists("A & B feat. C, a"), vec!["A", "B", "C"]);
        assert_eq!(option_name("A, B"), "A， B");
    }

    fn schema_from(json: &str) -> Schema {
        let props: HashMap<String, PropDef> = serde_json::from_str(json).unwrap();
        resolve_schema(&props, &[None, None, None]).unwrap()
    }

    #[test]
    fn select_fields_become_tags() {
        let schema = schema_from(
            r#"{
                "歌名": { "type": "title" },
                "歌手": { "type": "select", "select": { "options": [{ "name": "A", "color": "red" }] } },
                "語言": { "type": "select", "select": { "options": [{ "name": "中文", "color": "blue" }, { "name": "English" }] } },
                "分類": { "type": "multi_select", "multi_select": { "options": [{ "name": "抒情" }, { "name": "快歌" }] } },
                "備註": { "type": "rich_text" }
            }"#,
        );
        // 歌手欄位即使是 Select 也不當作標籤
        let names: Vec<&str> = schema.tags().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["分類", "語言"]);
        assert!(schema.tags()[0].multi);
        assert_eq!(schema.tags()[1].options[0].color, "blue");

        let mut warnings = Vec::new();
        let tag = |p: &str, v: &[&str]| Tag { property: p.into(), values: v.iter().map(|s| s.to_string()).collect() };
        let props = tag_properties(
            &schema,
            &[
                tag("語言", &["中文", "English"]),
                tag("分類", &["抒情", "不存在", "抒情"]),
                tag("沒有這欄", &["x"]),
                tag("歌手", &["A"]),
            ],
            &mut warnings,
        );
        assert_eq!(props["語言"], json!({ "select": { "name": "中文" } }));
        assert_eq!(props["分類"], json!({ "multi_select": [{ "name": "抒情" }] }));
        assert!(!props.contains_key("歌手"));
        assert_eq!(warnings.len(), 4);
    }

    #[test]
    fn text_fields_and_links() {
        let schema = schema_from(
            r#"{
                "歌名": { "type": "title" },
                "歌手": { "type": "rich_text" },
                "伴奏": { "type": "rich_text" },
                "備註": { "type": "rich_text" }
            }"#,
        );
        assert_eq!(schema.text_fields(), ["伴奏", "備註"]);

        let mut warnings = Vec::new();
        let tv = |p: &str, t: &str, l: Option<&str>| TextValue {
            property: p.into(),
            text: t.into(),
            link: l.map(String::from),
        };
        let props = text_properties(
            &schema,
            &[
                tv("伴奏", "和聲伴奏", Some("https://youtu.be/x")),
                tv("備註", " 原KEY ", Some("javascript:alert(1)")),
                tv("歌手", "x", None),
            ],
            &mut warnings,
        );
        assert_eq!(props["伴奏"]["rich_text"][0]["text"]["link"]["url"], "https://youtu.be/x");
        assert_eq!(props["備註"]["rich_text"][0]["text"], json!({ "content": "原KEY" }));
        assert!(!props.contains_key("歌手"));
        assert_eq!(warnings.len(), 2);
    }

    #[test]
    fn bilingual_format_matches_database() {
        let blocks = bilingual_blocks("<Verse 1>\n早安\nGood morning\n\n天氣很好\nThe weather is nice");
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["paragraph", "heading_2", "paragraph", "paragraph", "heading_2", "paragraph"]);
        assert_eq!(blocks[0]["paragraph"]["rich_text"][0]["annotations"]["bold"], true);
        assert_eq!(blocks[2]["paragraph"]["rich_text"][0]["annotations"]["color"], "orange");
        assert_eq!(blocks[3]["paragraph"]["rich_text"], json!([]));
    }

    #[test]
    fn blocks_per_stanza() {
        assert_eq!(lyrics_blocks("a\nb\n\nc\n\n\n").len(), 2);
    }
}
