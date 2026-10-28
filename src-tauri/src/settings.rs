//! 使用者設定。非機密設定存成 JSON；API Key 只存在作業系統的認證管理員。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "lyrics-fetcher";
const KEYRING_USER: &str = "notion-api-key";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub database_id: String,
    pub artist_property: String,
    pub lyrics_property: String,
    pub url_property: String,
    pub title_selector: String,
    pub artist_selector: String,
    pub lyrics_selector: String,
    pub write_body: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            database_id: String::new(),
            artist_property: String::new(),
            lyrics_property: String::new(),
            url_property: String::new(),
            title_selector: String::new(),
            artist_selector: String::new(),
            lyrics_selector: String::new(),
            write_body: true,
        }
    }
}

/// 讀不到或格式錯誤時回傳預設值（第一次啟動屬正常情況）
pub fn load(path: &Path) -> Settings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// 先寫暫存檔再改名，避免寫到一半當機留下損壞的設定檔
pub fn save(path: &Path, settings: &Settings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("無法建立設定資料夾：{e}"))?;
    }
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("無法寫入設定：{e}"))?;
    fs::rename(&tmp, path).map_err(|e| format!("無法寫入設定：{e}"))
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| format!("無法存取系統認證管理員：{e}"))
}

pub fn get_api_key() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("無法讀取 API Key：{e}")),
    }
}

pub fn set_api_key(key: &str) -> Result<(), String> {
    entry()?
        .set_password(key)
        .map_err(|e| format!("無法儲存 API Key：{e}"))
}

pub fn delete_api_key() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("無法刪除 API Key：{e}")),
    }
}
