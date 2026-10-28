// 宣告 app 指令清單，讓自訂指令也經過 capabilities 檢查（只授權 main 視窗）。
// 沒有這份清單時，Tauri 對本機來源一律放行，被導回 app 頁面的擷取視窗也能呼叫所有指令。
const COMMANDS: &[&str] = &[
    "get_settings",
    "save_settings",
    "test_connection",
    "get_schema",
    "field_suggestions",
    "search_lyrics",
    "search_videos",
    "search_lyricstranslate",
    "search_kkbox",
    "to_bilingual",
    "combine_lyrics",
    "convert_chinese",
    "fetch_song",
    "capture_song",
    "import_song",
    "open_link",
    "check_update",
    "restart_app",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("tauri build 失敗");
}
