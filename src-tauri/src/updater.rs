//! 自動更新：開啟時檢查 GitHub Releases 的最新版本，有新版就下載、驗證簽章並替換執行檔。
//!
//! 發布流程（見 README「發布新版本」）：zip 以 `cargo tauri signer sign` 簽章，`.zip.sig` 一起上傳。
//! 程式只接受以內嵌公鑰驗證通過的 zip，即使下載來源被竄改也不會安裝。
//!
//! Windows 允許把執行中的 exe 改名但不能覆寫，所以先把舊檔改名為 `.old`、寫入新檔，
//! 重新啟動後即為新版，`.old` 在下次啟動時刪除。

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use lyrics_fetcher::{HttpClient, Url};
use serde::Serialize;
use serde_json::Value;

const RELEASES_API: &str = "https://api.github.com/repos/SamoyedQQ/LyricsFetcher/releases/latest";
/// `cargo tauri signer generate` 產生的公鑰（.pub 檔內容）；私鑰只存在發布者的電腦
const PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEJEMzVCM0U5MTk0ODRDQ0MKUldUTVRFZ1o2Yk0xdmV2U3QvV2t0bDNKRUdRaENWWkFhcjFnZVo2VVg1eUp3VkZWUXJ4QUlhaVoK";
const ASSET_SUFFIX: &str = "-windows-x64.zip";
/// zip 內要更新的執行檔（GUI 必定更新；CLI 有放在同資料夾才更新）
const GUI_EXE: &str = "lyrics-fetcher-gui.exe";
const CLI_EXE: &str = "lyrics-fetcher.exe";
const MAX_ZIP_BYTES: usize = 64 * 1024 * 1024;
const MAX_SIG_BYTES: usize = 16 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    /// 已下載並替換的新版本；None = 已是最新版
    pub updated: Option<String>,
}

/// `v0.2.0` / `0.2.0` → (0, 2, 0)
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split(['.', '-', '+']);
    let mut next = || parts.next()?.parse().ok();
    Some((next()?, next()?, next()?))
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

/// 刪除上次更新留下的舊執行檔（啟動時呼叫，失敗無妨）
pub fn cleanup() {
    if let Ok(exe) = std::env::current_exe() {
        for path in [exe.clone(), exe.with_file_name(CLI_EXE)] {
            let _ = std::fs::remove_file(old_path(&path));
        }
    }
}

/// 只接受 GitHub 的 https 網址（下載會再轉址到 GitHub 的檔案主機，由 reqwest 跟隨）
fn github_url(s: &str) -> Result<Url, String> {
    let u = Url::parse(s).map_err(|_| "更新檔網址格式不正確".to_string())?;
    let host = u.host_str().unwrap_or_default();
    if u.scheme() == "https" && (host == "github.com" || host == "api.github.com") {
        Ok(u)
    } else {
        Err(format!("拒絕從非 GitHub 網址下載更新：{s}"))
    }
}

async fn download(client: &HttpClient, url: Url, limit: usize) -> Result<Vec<u8>, String> {
    let mut resp = client
        .get(url)
        .header("Accept", "application/octet-stream")
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("下載更新失敗：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下載更新失敗：HTTP {}", resp.status().as_u16()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| format!("下載更新失敗：{e}"))? {
        if body.len() + chunk.len() > limit {
            return Err("更新檔過大".into());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 以內嵌公鑰驗證 minisign 簽章。`.sig` 與公鑰都是 `cargo tauri signer` 輸出的 base64 文字。
fn verify(data: &[u8], sig_b64: &[u8]) -> Result<(), String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let decode = |s: &[u8]| -> Result<String, String> {
        let bytes = b64.decode(s.trim_ascii()).map_err(|_| "簽章格式錯誤".to_string())?;
        String::from_utf8(bytes).map_err(|_| "簽章格式錯誤".to_string())
    };
    let key_text = decode(PUBLIC_KEY.as_bytes())?;
    let key_line = key_text.lines().last().unwrap_or_default();
    let key = minisign_verify::PublicKey::from_base64(key_line).map_err(|e| format!("公鑰錯誤：{e}"))?;
    let sig = minisign_verify::Signature::decode(&decode(sig_b64)?).map_err(|e| format!("簽章格式錯誤：{e}"))?;
    key.verify(data, &sig, false).map_err(|_| "更新檔簽章驗證失敗，已拒絕安裝".to_string())
}

/// zip 中檔名為 `name` 的檔案（不論在哪個資料夾）
fn extract(zip: &[u8], name: &str) -> Result<Option<Vec<u8>>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip)).map_err(|e| format!("更新檔損毀：{e}"))?;
    for i in 0..archive.len() {
        let file = archive.by_index(i).map_err(|e| format!("更新檔損毀：{e}"))?;
        let matches = file.name().is_ok_and(|n| n.rsplit(['/', '\\']).next() == Some(name));
        if file.is_file() && matches {
            let mut out = Vec::with_capacity(file.size() as usize);
            file.take(MAX_ZIP_BYTES as u64 * 2).read_to_end(&mut out).map_err(|e| format!("更新檔損毀：{e}"))?;
            return Ok(Some(out));
        }
    }
    Ok(None)
}

/// 把 `path` 換成新內容：舊檔改名為 `.old`（執行中的 exe 也可以改名），寫入失敗時還原
fn replace(path: &Path, data: &[u8]) -> Result<(), String> {
    let old = old_path(path);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, &old).map_err(|e| format!("無法替換 {}：{e}", path.display()))?;
    if let Err(e) = std::fs::write(path, data) {
        let _ = std::fs::rename(&old, path);
        return Err(format!("無法寫入新版：{e}"));
    }
    Ok(())
}

/// 檢查並安裝更新。開發版（debug）不檢查，避免覆寫 target/debug 的執行檔。
pub async fn check_and_install(client: &HttpClient) -> Result<UpdateStatus, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let status = |updated| UpdateStatus { current: current.clone(), updated };
    if cfg!(debug_assertions) {
        return Ok(status(None));
    }

    let resp = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("檢查更新失敗：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("檢查更新失敗：HTTP {}", resp.status().as_u16()));
    }
    let release: Value = resp.json().await.map_err(|e| format!("檢查更新失敗：{e}"))?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let (Some(latest), Some(cur)) = (parse_version(tag), parse_version(&current)) else {
        return Ok(status(None));
    };
    if latest <= cur {
        return Ok(status(None));
    }

    let empty = Vec::new();
    let assets = release["assets"].as_array().unwrap_or(&empty);
    let asset_url = |pred: &dyn Fn(&str) -> bool| {
        assets
            .iter()
            .find(|a| pred(a["name"].as_str().unwrap_or_default()))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(str::to_string)
    };
    let zip_url = asset_url(&|n| n.ends_with(ASSET_SUFFIX)).ok_or("新版沒有 Windows 下載檔")?;
    let sig_url = asset_url(&|n| n.ends_with(&format!("{ASSET_SUFFIX}.sig"))).ok_or("新版沒有簽章檔，已略過更新")?;

    let sig = download(client, github_url(&sig_url)?, MAX_SIG_BYTES).await?;
    let zip = download(client, github_url(&zip_url)?, MAX_ZIP_BYTES).await?;
    verify(&zip, &sig)?;

    let gui = extract(&zip, GUI_EXE)?.ok_or("更新檔中沒有主程式")?;
    let exe = std::env::current_exe().map_err(|e| format!("找不到目前的執行檔：{e}"))?;
    replace(&exe, &gui)?;
    // CLI 只在使用者有放在同一個資料夾時更新
    let cli_path = exe.with_file_name(CLI_EXE);
    if cli_path.exists()
        && let Some(cli) = extract(&zip, CLI_EXE)?
    {
        let _ = replace(&cli_path, &cli);
    }
    Ok(status(Some(tag.trim_start_matches('v').to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("1.10.3-beta"), Some((1, 10, 3)));
        assert!(parse_version("v0.10.0") > parse_version("0.9.9"));
        assert_eq!(parse_version("latest"), None);
    }

    #[test]
    fn only_github_urls() {
        assert!(github_url("https://github.com/SamoyedQQ/LyricsFetcher/releases/download/v1/a.zip").is_ok());
        assert!(github_url("http://github.com/a").is_err());
        assert!(github_url("https://evil.example/a.zip").is_err());
        assert!(github_url("https://github.com.evil.example/a.zip").is_err());
    }

    /// 以 `cargo tauri signer sign` 實際簽過的檔案驗證：發布流程與驗證程式碼相容
    #[test]
    fn verifies_signed_file() {
        let (Ok(data), Ok(sig)) = (std::env::var("UPDATER_TEST_FILE"), std::env::var("UPDATER_TEST_SIG")) else {
            return; // 需要私鑰才能產生，平常略過
        };
        let data = std::fs::read(data).unwrap();
        let sig = std::fs::read(sig).unwrap();
        assert!(verify(&data, &sig).is_ok());
        if data.starts_with(b"PK") {
            assert!(extract(&data, GUI_EXE).unwrap().is_some_and(|exe| exe.starts_with(b"MZ")));
        }
        let mut tampered = data.clone();
        tampered.push(b'!');
        assert!(verify(&tampered, &sig).is_err());
    }

    #[test]
    fn rejects_bad_signatures() {
        assert!(verify(b"data", b"not base64!").is_err());
        assert!(PUBLIC_KEY.len() > 40);
    }
}
