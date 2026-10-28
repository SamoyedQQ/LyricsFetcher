// release 版不顯示主控台視窗
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod settings;
mod updater;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lyrics_fetcher::bilingual;
use lyrics_fetcher::crawler::{self, SelectorOverrides, Song};
use lyrics_fetcher::error::AppError;
use lyrics_fetcher::genius::{self, Fetched};
use lyrics_fetcher::notion::{self, FieldValues, Notion, NotionConfig, PageOptions, Schema, Tag, TagField, TextValue};
use lyrics_fetcher::search::{self, LyricsHit, LyricsResults, VideoHit};
use lyrics_fetcher::{HttpClient, Url, zh};
use serde::{Deserialize, Serialize};
use settings::Settings;
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_opener::OpenerExt;
use tokio::sync::oneshot;

/// 前端送來的歌詞上限，防止異常輸入佔用大量記憶體
const MAX_LYRICS_BYTES: usize = 1024 * 1024;

/// 擷取視窗用來回傳資料的保留網域（RFC 2606 .invalid，永遠不會真的連線）
const CAPTURE_HOST: &str = "capture.lyricsfetcher.invalid";
const CAPTURE_LABEL: &str = "capture";
const CAPTURE_SCRIPT: &str = include_str!("capture.js");
/// LyricsTranslate / KKBOX 搜尋視窗（與擷取視窗分開，可同時進行）
const LT_SEARCH_LABEL: &str = "lt-search";
const LT_SEARCH_SCRIPT: &str = include_str!("search_capture.js");
const KKBOX_SEARCH_LABEL: &str = "kkbox-search";
const KKBOX_SEARCH_SCRIPT: &str = include_str!("kkbox_search.js");
/// 包含使用者完成人機驗證的時間
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(180);
/// 瀏覽模式需要使用者自己搜尋、點選，給比較長的時間
const BROWSE_TIMEOUT: Duration = Duration::from_secs(600);
/// 匯入時文字欄位的數量與長度上限
const MAX_TEXT_FIELDS: usize = 20;
const MAX_TEXT_CHARS: usize = 2000;

struct AppState {
    client: HttpClient,
    settings_path: PathBuf,
    settings: Mutex<Settings>,
    /// (資料庫 ID, schema)：同一資料庫連續匯入時省下一次 API 往返；設定變更時清除
    schema: Mutex<Option<(String, Schema)>>,
    /// (資料庫 ID, 文字欄位常用值)：需掃描數百筆頁面，同一資料庫只讀一次
    suggestions: Mutex<Option<(String, Vec<FieldValues>)>>,
}

type CmdResult<T> = Result<T, String>;

fn err(e: AppError) -> String {
    e.to_string()
}

impl AppState {
    // 鎖只在同步區塊內短暫持有，絕不跨越 await
    fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn cached_schema(&self, db: &str) -> Option<Schema> {
        let guard = self.schema.lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().filter(|(id, _)| id == db).map(|(_, s)| s.clone())
    }

    fn store_schema(&self, db: &str, schema: &Schema) {
        *self.schema.lock().unwrap_or_else(|p| p.into_inner()) = Some((db.to_string(), schema.clone()));
    }

    fn clear_schema(&self) {
        *self.schema.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.suggestions.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    fn notion(&self) -> CmdResult<Notion> {
        let s = self.settings();
        let api_key = settings::get_api_key()?.unwrap_or_default();
        if api_key.is_empty() || s.database_id.trim().is_empty() {
            return Err("尚未完成設定：請在設定中填入 Notion API Key 與 Database ID".into());
        }
        let opt = |v: &str| Some(v.to_string()).filter(|v| !v.trim().is_empty());
        let cfg = NotionConfig {
            api_key,
            database_id: s.database_id.clone(),
            version: None,
            artist_property: opt(&s.artist_property),
            lyrics_property: opt(&s.lyrics_property),
            url_property: opt(&s.url_property),
        };
        Notion::new(self.client.clone(), &cfg).map_err(err)
    }

    async fn schema_for(&self, notion: &Notion) -> CmdResult<Schema> {
        if let Some(s) = self.cached_schema(notion.database_id()) {
            return Ok(s);
        }
        let schema = notion.fetch_schema().await.map_err(err)?;
        self.store_schema(notion.database_id(), &schema);
        Ok(schema)
    }
}

// ─── 設定 ───────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    #[serde(flatten)]
    settings: Settings,
    /// 只告訴前端「有沒有」金鑰，金鑰本身永遠不離開後端
    has_api_key: bool,
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> CmdResult<SettingsView> {
    Ok(SettingsView {
        settings: state.settings(),
        has_api_key: settings::get_api_key()?.is_some(),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SaveInput {
    #[serde(flatten)]
    settings: Settings,
    /// None 或空字串 = 保留現有金鑰
    api_key: Option<String>,
    #[serde(default)]
    clear_api_key: bool,
}

#[tauri::command]
fn save_settings(state: State<'_, AppState>, input: SaveInput) -> CmdResult<SettingsView> {
    let mut s = input.settings;
    for field in [
        &mut s.database_id,
        &mut s.artist_property,
        &mut s.lyrics_property,
        &mut s.url_property,
        &mut s.title_selector,
        &mut s.artist_selector,
        &mut s.lyrics_selector,
    ] {
        *field = field.trim().to_string();
    }
    if !s.database_id.is_empty() && notion::normalize_id(&s.database_id).is_none() {
        return Err("Database ID 格式不正確：請貼上 32 碼 ID 或資料庫網址".into());
    }
    selector_overrides(&s)?;

    if input.clear_api_key {
        settings::delete_api_key()?;
    } else if let Some(key) = input.api_key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty()) {
        notion::validate_api_key(&key).map_err(err)?;
        settings::set_api_key(&key)?;
    }

    settings::save(&state.settings_path, &s)?;
    *state.settings.lock().unwrap_or_else(|p| p.into_inner()) = s;
    state.clear_schema();
    get_settings(state)
}

fn selector_overrides(s: &Settings) -> CmdResult<SelectorOverrides> {
    let opt = |v: &str| Some(v.to_string());
    SelectorOverrides::new(opt(&s.title_selector), opt(&s.artist_selector), opt(&s.lyrics_selector)).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SchemaInfo {
    summary: String,
    warnings: Vec<String>,
    tags: Vec<TagField>,
    /// 可填寫的 Text 欄位（伴奏、備註…）
    texts: Vec<String>,
}

impl From<Schema> for SchemaInfo {
    fn from(s: Schema) -> Self {
        Self {
            summary: s.summary(),
            tags: s.tags().to_vec(),
            texts: s.text_fields().to_vec(),
            warnings: s.warnings,
        }
    }
}

#[tauri::command]
async fn test_connection(state: State<'_, AppState>) -> CmdResult<SchemaInfo> {
    let notion = state.notion()?;
    // 測試連線一律重新讀取，不使用快取
    let schema = notion.fetch_schema().await.map_err(err)?;
    state.store_schema(notion.database_id(), &schema);
    Ok(schema.into())
}

/// 資料庫的標籤欄位（有快取時不會發出請求）
#[tauri::command]
async fn get_schema(state: State<'_, AppState>) -> CmdResult<SchemaInfo> {
    let notion = state.notion()?;
    Ok(state.schema_for(&notion).await?.into())
}

/// 各文字欄位在資料庫中常用的值，作為輸入建議
#[tauri::command]
async fn field_suggestions(state: State<'_, AppState>) -> CmdResult<Vec<FieldValues>> {
    let notion = state.notion()?;
    let db = notion.database_id().to_string();
    let cached = {
        let guard = state.suggestions.lock().unwrap_or_else(|p| p.into_inner());
        guard.as_ref().filter(|(id, _)| *id == db).map(|(_, v)| v.clone())
    };
    if let Some(v) = cached {
        return Ok(v);
    }
    let schema = state.schema_for(&notion).await?;
    let values = notion.text_suggestions(&schema).await.map_err(err)?;
    *state.suggestions.lock().unwrap_or_else(|p| p.into_inner()) = Some((db, values.clone()));
    Ok(values)
}

// ─── 搜尋 ───────────────────────────────────────────────────────────────

#[tauri::command]
async fn search_lyrics(state: State<'_, AppState>, query: String) -> CmdResult<LyricsResults> {
    search::search_lyrics(&state.client, &query).await.map_err(err)
}

#[tauri::command]
async fn search_videos(state: State<'_, AppState>, query: String) -> CmdResult<Vec<VideoHit>> {
    search::search_videos(&state.client, &query).await.map_err(err)
}

/// 簡繁轉換：`to` 為 "hant"（台灣繁體）或 "hans"（簡體）
#[tauri::command]
fn convert_chinese(text: String, to: String) -> CmdResult<String> {
    if text.len() > MAX_LYRICS_BYTES {
        return Err("內容過長".into());
    }
    match to.as_str() {
        "hant" => Ok(zh::to_traditional(&text)),
        "hans" => Ok(zh::to_simplified(&text)),
        _ => Err("不支援的轉換方向".into()),
    }
}

/// 手動切換中英格式時，把目前的歌詞轉成「一行原文、一行英譯」；偵測不到英譯回傳 None
#[tauri::command]
fn to_bilingual(lyrics: String) -> CmdResult<Option<String>> {
    if lyrics.len() > MAX_LYRICS_BYTES {
        return Err("歌詞內容過長".into());
    }
    Ok(bilingual::to_bilingual(&lyrics))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LyricsSource {
    lyrics: String,
    bilingual: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CombinedDto {
    /// 歌名、歌手、來源網址取自第幾份
    primary: usize,
    lyrics: String,
    bilingual: Option<String>,
    note: String,
}

/// 任選兩份歌詞合併成中英對照（自動判斷原文與英譯；語言重複時去重）
#[tauri::command]
fn combine_lyrics(first: LyricsSource, second: LyricsSource) -> CmdResult<CombinedDto> {
    let all = [&first, &second];
    if all.iter().any(|s| s.lyrics.len() + s.bilingual.as_ref().map_or(0, String::len) > MAX_LYRICS_BYTES) {
        return Err("歌詞內容過長".into());
    }
    let sources = all.map(|s| bilingual::Source {
        lyrics: &s.lyrics,
        bilingual: s.bilingual.as_deref(),
    });
    let c = bilingual::combine(sources);
    Ok(CombinedDto {
        primary: c.primary,
        lyrics: c.lyrics,
        bilingual: c.bilingual,
        note: c.note,
    })
}

// ─── 抓取與匯入 ─────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SongDto {
    title: String,
    artist: String,
    lyrics: String,
    source_url: String,
}

impl From<Song> for SongDto {
    fn from(s: Song) -> Self {
        Self {
            title: s.title,
            artist: s.artist,
            lyrics: s.lyrics,
            source_url: s.source_url,
        }
    }
}

/// 預覽用：附上中英對照版本（來源附英譯，或 Genius 合併了英文翻譯頁）
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewDto {
    #[serde(flatten)]
    song: SongDto,
    bilingual: Option<String>,
    /// 給使用者的說明，例如「已合併 Genius 英文翻譯」
    note: Option<String>,
}

impl From<Fetched> for PreviewDto {
    fn from(f: Fetched) -> Self {
        Self {
            song: f.song.into(),
            bilingual: f.bilingual,
            note: f.note,
        }
    }
}

#[tauri::command]
async fn fetch_song(state: State<'_, AppState>, url: String) -> CmdResult<PreviewDto> {
    let url = url.trim().to_string();
    crawler::validate_url(&url).map_err(err)?;
    let selectors = selector_overrides(&state.settings())?;

    // 抓網頁的同時預先讀取 schema，按下匯入時就不必再等；schema 失敗不影響預覽
    let prefetch = async {
        if let Ok(notion) = state.notion() {
            let _ = state.schema_for(&notion).await;
        }
    };
    // Genius 會一併抓英文翻譯頁並合併
    let (fetched, ()) = tokio::join!(genius::fetch_any(&state.client, &url, &selectors), prefetch);
    Ok(fetched.map_err(err)?.into())
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CapturePayload {
    title: String,
    artist: String,
    /// document.title，LyricsTranslate 以此解析歌名與歌手
    page_title: String,
    lyrics_html: String,
    lyrics_text: String,
    /// 左右對照頁（LyricsTranslate）每一行在頁面上的所有文字
    rows: Vec<ColumnRow>,
    error: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ColumnRow {
    p: u32,
    /// 原文、譯文（順序不一定，也可能缺少或重複），由 `bilingual::pick_columns` 判斷
    texts: Vec<String>,
}

/// 左右對照頁最多處理的行數
const MAX_ROWS: usize = 2000;

type CaptureSender<T> = Arc<Mutex<Option<oneshot::Sender<CmdResult<T>>>>>;

/// 只送出一次：成功、失敗、關閉視窗三者先到先得
fn send_once<T>(tx: &CaptureSender<T>, result: CmdResult<T>) {
    if let Some(tx) = tx.lock().unwrap_or_else(|p| p.into_inner()).take() {
        let _ = tx.send(result);
    }
}

/// 背景中的擷取 / 搜尋視窗要求顯示（需要人機驗證或載入過久）。
/// 在背景工作中操作視窗，避免在 WebView 回呼中同步呼叫。
fn show_window(app: &tauri::AppHandle, label: &'static str) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(w) = app.get_webview_window(label) {
            let _ = w.show();
            let _ = w.set_focus();
        }
    });
}

/// 外部頁面不得把擷取 / 搜尋視窗導向 app 本機頁面（tauri.localhost 等），否則會取得本機來源
fn is_local_host(url: &Url) -> bool {
    url.host_str().is_some_and(|h| h == "localhost" || h.ends_with(".localhost"))
}

fn decode_capture<T: serde::de::DeserializeOwned>(url: &Url) -> CmdResult<T> {
    let data = url
        .query_pairs()
        .find(|(k, _)| k == "d")
        .map(|(_, v)| v.into_owned())
        .ok_or("擷取資料缺失")?;
    if data.len() > MAX_LYRICS_BYTES {
        return Err("擷取的內容過大".into());
    }
    serde_json::from_str(&data).map_err(|e| format!("擷取資料格式錯誤：{e}"))
}

/// 以內嵌瀏覽器（WebView2）載入頁面並擷取歌詞，用於需要瀏覽器驗證的網站（如 KKBOX）。
///
/// 視窗可見：若網站要求人機驗證，由使用者自行完成。
/// 外部頁面沒有任何 IPC 權限：capabilities 只授權 main 視窗，且不允許導向 app 本機頁面。
///
/// `columns_only`：瀏覽模式。使用者在視窗中自行搜尋並點進 LyricsTranslate 的翻譯頁，
/// 出現左右對照的歌詞時才擷取。
#[tauri::command]
async fn capture_song(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    url: String,
    columns_only: Option<bool>,
) -> CmdResult<PreviewDto> {
    let columns_only = columns_only.unwrap_or(false);
    let url = url.trim().to_string();
    crawler::validate_url(&url).map_err(err)?;
    let target = Url::parse(&url).map_err(|_| "網址格式不正確".to_string())?;

    // 同時只允許一個擷取視窗
    if let Some(old) = app.get_webview_window(CAPTURE_LABEL) {
        let _ = old.destroy();
    }

    let selector = Some(state.settings().lyrics_selector).filter(|s| !s.trim().is_empty());
    // serde_json 輸出的字串字面值同時是合法的 JS，可安全嵌入腳本
    let selector_js = serde_json::to_string(&selector).map_err(|e| e.to_string())?;
    let script = CAPTURE_SCRIPT
        .replace("__LYRICS_SELECTOR__", &selector_js)
        .replace("__COLUMNS_ONLY__", if columns_only { "true" } else { "false" });

    let (tx, rx) = oneshot::channel();
    let tx: CaptureSender<CapturePayload> = Arc::new(Mutex::new(Some(tx)));
    let tx_nav = tx.clone();
    let app_nav = app.clone();

    // 一般擷取在背景進行（視窗隱藏）；需要人機驗證或載入過久時，頁面腳本會要求顯示視窗。
    // 瀏覽模式需要使用者操作，一開始就顯示。
    let window = WebviewWindowBuilder::new(&app, CAPTURE_LABEL, WebviewUrl::External(target))
        .title(if columns_only {
            "請點選 LyricsTranslate 的英文翻譯頁 · 載入後自動擷取"
        } else {
            "擷取歌詞 · 若出現驗證請在此完成"
        })
        .inner_size(960.0, 760.0)
        .center()
        .visible(columns_only)
        .initialization_script(&script)
        .on_navigation(move |nav| {
            if is_local_host(nav) {
                return false;
            }
            if nav.host_str() != Some(CAPTURE_HOST) {
                return true;
            }
            if nav.path() == "/show" {
                show_window(&app_nav, CAPTURE_LABEL);
            } else {
                send_once(&tx_nav, decode_capture(nav));
            }
            false // 取消導向：保留網域本來就不存在
        })
        .build()
        .map_err(|e| format!("無法開啟擷取視窗：{e}"))?;

    let tx_close = tx.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            send_once(&tx_close, Err("已取消：擷取視窗被關閉".into()));
        }
    });

    let limit = if columns_only { BROWSE_TIMEOUT } else { CAPTURE_TIMEOUT };
    let outcome = tokio::time::timeout(limit, rx).await;
    let _ = window.destroy();
    let payload = match outcome {
        Err(_) => return Err("擷取逾時：頁面未在時間內載入歌詞".into()),
        Ok(Err(_)) => return Err("已取消".into()),
        Ok(Ok(result)) => result?,
    };

    if let Some(e) = payload.error {
        return Err(format!("{e}\n可在設定中填入「歌詞 selector」，或改用手動輸入"));
    }

    let (mut title, mut artist) = (crawler::clean_title(&payload.title), crawler::clean_artist(&payload.artist));
    // LyricsTranslate 的 <h1> 可能是譯文標題，改用頁面標題「歌手 - 歌名 (English translation)」
    if let Some((t, a, _)) = search::lyricstranslate_title(&payload.page_title) {
        (title, artist) = (t, a);
    }

    if !payload.rows.is_empty() {
        let rows: Vec<(u32, String, String)> = payload
            .rows
            .into_iter()
            .take(MAX_ROWS)
            .map(|r| {
                let (orig, tr) = bilingual::pick_columns(&r.texts);
                (r.p, orig, tr)
            })
            .collect();
        let (lyrics, both) = bilingual::from_columns(&rows);
        if lyrics.is_empty() {
            return Err("擷取到的歌詞是空的".into());
        }
        let note = match &both {
            Some(_) => "已合併 LyricsTranslate 的原文與英文翻譯".to_string(),
            // 原文欄只有拼音或沒載入：只拿到英譯
            None if !bilingual::has_cjk(&lyrics) => {
                "此頁只取得英文翻譯（原文可能只有拼音）；可在搜尋結果按「＋」再選一個中文歌詞自動合併".to_string()
            }
            None => "此頁沒有英文翻譯，只匯入原文；可在搜尋結果按「＋」再選一個英文翻譯自動合併".to_string(),
        };
        return Ok(Fetched {
            song: Song { title, artist, lyrics, source_url: url },
            bilingual: both,
            note: Some(note),
        }
        .into());
    }

    // 優先用 HTML 片段（保留 <br> / <p> 結構），失敗才用純文字
    let mut lyrics = crawler::lyrics_from_fragment(&payload.lyrics_html);
    if lyrics.is_empty() {
        lyrics = crawler::clean_lyrics(&payload.lyrics_text);
    }
    if lyrics.is_empty() {
        return Err("擷取到的歌詞是空的".into());
    }

    Ok(Fetched::from(Song {
        title,
        artist,
        lyrics,
        source_url: url,
    })
    .into())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchPayload {
    links: Vec<SearchLink>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchLink {
    href: String,
    text: String,
}

/// 搜尋關鍵字：合併空白、限制長度
fn search_query(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect()
}

/// 在隱藏的內嵌瀏覽器中載入 `target`，由注入的 `script` 讀取結果並以導向保留網域的方式送回。
/// 需要人機驗證時腳本會要求顯示視窗，由使用者完成。同一 `label` 的新搜尋取代舊的。
async fn browser_search<T: serde::de::DeserializeOwned + Send + 'static>(
    app: &tauri::AppHandle,
    label: &'static str,
    title: &str,
    target: Url,
    script: &str,
) -> CmdResult<T> {
    if let Some(old) = app.get_webview_window(label) {
        let _ = old.destroy();
    }

    let (tx, rx) = oneshot::channel();
    let tx: CaptureSender<T> = Arc::new(Mutex::new(Some(tx)));
    let tx_nav = tx.clone();
    let app_nav = app.clone();
    let window = WebviewWindowBuilder::new(app, label, WebviewUrl::External(target))
        .title(title)
        .inner_size(900.0, 720.0)
        .center()
        .visible(false)
        .initialization_script(script)
        .on_navigation(move |nav| {
            if is_local_host(nav) {
                return false;
            }
            if nav.host_str() != Some(CAPTURE_HOST) {
                return true;
            }
            if nav.path() == "/show" {
                show_window(&app_nav, label);
            } else {
                send_once(&tx_nav, decode_capture(nav));
            }
            false
        })
        .build()
        .map_err(|e| format!("無法開啟搜尋視窗：{e}"))?;

    let tx_close = tx.clone();
    window.on_window_event(move |event| {
        if matches!(event, WindowEvent::Destroyed) {
            send_once(&tx_close, Err("已取消".into()));
        }
    });

    let outcome = tokio::time::timeout(CAPTURE_TIMEOUT, rx).await;
    let _ = window.destroy();
    match outcome {
        Err(_) => Err("搜尋逾時".into()),
        Ok(Err(_)) => Err("已取消".into()),
        Ok(Ok(result)) => result,
    }
}

/// 在隱藏的內嵌瀏覽器中搜尋 LyricsTranslate。
///
/// LyricsTranslate（Cloudflare）與 DuckDuckGo 都會擋程式直接發出的請求，
/// 但一般瀏覽器可以正常使用；因此交給 WebView2 載入 DuckDuckGo 的 site: 搜尋頁。
#[tauri::command]
async fn search_lyricstranslate(app: tauri::AppHandle, query: String) -> CmdResult<Vec<LyricsHit>> {
    let query = search_query(&query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let target = Url::parse_with_params(
        "https://html.duckduckgo.com/html/",
        &[("q", format!("site:lyricstranslate.com {query}"))],
    )
    .map_err(|e| e.to_string())?;
    let title = "搜尋 LyricsTranslate · 若出現驗證請在此完成";
    let payload: SearchPayload = browser_search(&app, LT_SEARCH_LABEL, title, target, LT_SEARCH_SCRIPT).await?;
    let links: Vec<(String, String)> = payload.links.into_iter().take(50).map(|l| (l.href, l.text)).collect();
    Ok(search::lyricstranslate_hits(&query, &links))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct KkboxPayload {
    /// 站內搜尋 API 的回應（只保留需要的欄位）
    data: serde_json::Value,
    error: Option<String>,
}

/// KKBOX 的 AWS WAF 擋下一般請求時（`LyricsResults::kkbox_blocked`），改在隱藏的內嵌瀏覽器中搜尋：
/// 載入 KKBOX 首頁讓驗證自動完成，再由頁面內的腳本呼叫同網域的搜尋 API。
#[tauri::command]
async fn search_kkbox(app: tauri::AppHandle, query: String) -> CmdResult<Vec<LyricsHit>> {
    let query = search_query(&query);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let target = Url::parse("https://www.kkbox.com/tw/tc/").map_err(|e| e.to_string())?;
    // serde_json 輸出的字串字面值同時是合法的 JS，可安全嵌入腳本
    let query_js = serde_json::to_string(&query).map_err(|e| e.to_string())?;
    let script = KKBOX_SEARCH_SCRIPT.replace("__QUERY__", &query_js);
    let title = "搜尋 KKBOX · 若出現驗證請在此完成";
    let payload: KkboxPayload = browser_search(&app, KKBOX_SEARCH_LABEL, title, target, &script).await?;
    if let Some(e) = payload.error {
        return Err(e);
    }
    Ok(search::kkbox_hits(&query, &payload.data))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportResult {
    url: String,
    blocks: usize,
    mapping: String,
    warnings: Vec<String>,
}

#[tauri::command]
async fn import_song(
    state: State<'_, AppState>,
    song: SongDto,
    tags: Vec<Tag>,
    texts: Vec<TextValue>,
    bilingual: bool,
    write_body: bool,
) -> CmdResult<ImportResult> {
    if song.lyrics.len() > MAX_LYRICS_BYTES {
        return Err("歌詞內容過長".into());
    }
    if texts.len() > MAX_TEXT_FIELDS
        || texts.iter().any(|t| {
            t.text.chars().count() > MAX_TEXT_CHARS || t.link.as_ref().is_some_and(|l| l.len() > MAX_TEXT_CHARS)
        })
    {
        return Err("欄位內容過長".into());
    }
    // 手動貼上的文字也走同一套整理（冪等，對已整理過的歌詞無影響）
    let song = Song {
        title: song.title.trim().to_string(),
        artist: song.artist.trim().to_string(),
        lyrics: crawler::clean_lyrics(&song.lyrics),
        source_url: song.source_url.trim().to_string(),
    };
    if song.title.is_empty() {
        return Err("歌名不能是空白".into());
    }
    if song.lyrics.is_empty() {
        return Err("歌詞不能是空白".into());
    }
    // 手動輸入可以沒有來源網址；有的話必須是 http(s)
    if !song.source_url.is_empty() {
        crawler::validate_url(&song.source_url).map_err(err)?;
    }

    let notion = state.notion()?;
    let schema = state.schema_for(&notion).await?;
    let opts = PageOptions {
        tags,
        texts,
        bilingual,
        write_body,
    };
    let created = match notion.create_page(&schema, &song, &opts).await {
        Ok(c) => c,
        Err(e) => {
            // 失敗可能源於資料庫結構已變更，清掉快取讓下次重新讀取
            state.clear_schema();
            return Err(err(e));
        }
    };

    let mut warnings = schema.warnings.clone();
    warnings.extend(created.warnings);
    Ok(ImportResult {
        url: created.url,
        blocks: created.blocks,
        mapping: schema.summary(),
        warnings,
    })
}

/// 開啟時檢查 GitHub 上的新版，有的話下載、驗證簽章並替換執行檔（重新啟動後生效）
#[tauri::command]
async fn check_update(state: State<'_, AppState>) -> CmdResult<updater::UpdateStatus> {
    updater::check_and_install(&state.client).await
}

/// 更新完成後重新啟動，改執行新版
#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    app.restart();
}

/// 只允許開啟 Notion 與 YouTube 的 https 網址，避免前端被誘導開啟任意程式或檔案
#[tauri::command]
fn open_link(app: tauri::AppHandle, url: String) -> CmdResult<()> {
    let parsed = Url::parse(&url).map_err(|_| "網址格式不正確".to_string())?;
    let host = parsed.host_str().unwrap_or_default();
    let allowed = ["notion.so", "notion.site", "notion.com", "youtube.com", "youtu.be"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if parsed.scheme() != "https" || !allowed {
        return Err("只能開啟 Notion 或 YouTube 網址".into());
    }
    app.opener()
        .open_url(parsed.as_str(), None::<&str>)
        .map_err(|e| format!("無法開啟瀏覽器：{e}"))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            updater::cleanup();
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let client = crawler::build_client().map_err(|e| e.to_string())?;
            app.manage(AppState {
                client,
                settings: Mutex::new(settings::load(&settings_path)),
                settings_path,
                schema: Mutex::new(None),
                suggestions: Mutex::new(None),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            test_connection,
            get_schema,
            field_suggestions,
            search_lyrics,
            search_videos,
            search_lyricstranslate,
            search_kkbox,
            to_bilingual,
            combine_lyrics,
            convert_chinese,
            fetch_song,
            capture_song,
            import_song,
            open_link,
            check_update,
            restart_app,
        ])
        .run(tauri::generate_context!())
        .expect("無法啟動應用程式");
}
