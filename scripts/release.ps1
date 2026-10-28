# 發布新版本：編譯 → 打包 zip → 以私鑰簽章 → 建立 GitHub Release
#
# 用法（在專案根目錄）：  powershell -ExecutionPolicy Bypass -File scripts\release.ps1
# 版本號取自 src-tauri\tauri.conf.json；發布前請先更新 Cargo.toml、src-tauri\Cargo.toml 與 tauri.conf.json 的版本並推送。
# 簽章私鑰預設在 %USERPROFILE%\.tauri\lyricsfetcher.key（可用環境變數 LYRICS_SIGNING_KEY 指定）。
# 沒有 .sig 簽章檔的版本，舊版程式不會自動更新。
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)

$version = (Get-Content src-tauri\tauri.conf.json -Raw -Encoding UTF8 | ConvertFrom-Json).version
$key = if ($env:LYRICS_SIGNING_KEY) { $env:LYRICS_SIGNING_KEY } else { "$env:USERPROFILE\.tauri\lyricsfetcher.key" }
if (-not (Test-Path $key)) { throw "找不到簽章私鑰：$key" }
$name = "LyricsFetcher-v$version-windows-x64.zip"
Write-Host "發布 v$version"

# 執行中的 GUI 會鎖住執行檔
Get-Process lyrics-fetcher-gui -ErrorAction SilentlyContinue | Stop-Process -Force
cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { throw "編譯失敗" }

$stage = "dist\LyricsFetcher"
if (Test-Path dist) { Remove-Item dist -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\lyrics-fetcher-gui.exe, target\release\lyrics-fetcher.exe, .env.example, LICENSE, packaging\* $stage

# 以 UTF-8 檔名打包（中文檔名在其他解壓程式也能正確顯示）
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::CreateFromDirectory((Resolve-Path $stage), "$PWD\dist\$name", [IO.Compression.CompressionLevel]::Optimal, $true, [Text.Encoding]::UTF8)

# 私鑰需有密碼：由環境變數 TAURI_SIGNING_PRIVATE_KEY_PASSWORD 提供，未設定時 signer 會詢問
cargo tauri signer sign -f $key "dist\$name"
if ($LASTEXITCODE -ne 0) { throw "簽章失敗" }

gh release create "v$version" "dist\$name" "dist\$name.sig" --title "Lyrics Fetcher v$version" --generate-notes
if ($LASTEXITCODE -ne 0) { throw "建立 Release 失敗" }
Write-Host "完成：v$version"
