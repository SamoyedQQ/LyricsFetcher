//! 極簡暗色系終端機介面。
//!
//! 進度與訊息寫到 stderr，最終結果（Notion 網址 / dry-run 歌詞）寫到 stdout，
//! 方便 `lyrics-fetcher URL > out.txt` 這類管線使用。
//! 設定 `NO_COLOR` 或輸出非終端機時自動停用顏色。

use std::fmt::Display;
use std::io::IsTerminal;
use std::sync::OnceLock;

// 低飽和真彩色，適合深色背景
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const MUTED: &str = "\x1b[38;2;108;112;134m";
const ACCENT: &str = "\x1b[38;2;137;180;250m";
const OK: &str = "\x1b[38;2;166;218;149m";
const WARN: &str = "\x1b[38;2;238;212;159m";
const ERR: &str = "\x1b[38;2;237;135;150m";

/// 標籤欄寬（以終端機顯示寬度計，中文字佔 2 欄）
const LABEL_WIDTH: usize = 16;

struct Caps {
    err_color: bool,
    out_color: bool,
}

fn caps() -> &'static Caps {
    static CAPS: OnceLock<Caps> = OnceLock::new();
    CAPS.get_or_init(|| {
        let allowed = std::env::var_os("NO_COLOR").is_none();
        Caps {
            err_color: allowed && std::io::stderr().is_terminal() && vt::enable(vt::STDERR),
            out_color: allowed && std::io::stdout().is_terminal() && vt::enable(vt::STDOUT),
        }
    })
}

fn paint(color: &str, text: impl Display) -> String {
    if caps().err_color {
        format!("{color}{text}{RESET}")
    } else {
        text.to_string()
    }
}

fn row(icon: &str, color: &str, label: &str, detail: impl Display) {
    let pad = LABEL_WIDTH.saturating_sub(display_width(label));
    eprintln!(
        "  {}  {}{:pad$}{}",
        paint(color, icon),
        label,
        "",
        paint(MUTED, detail)
    );
}

pub fn header() {
    eprintln!();
    eprintln!(
        "  {}  {}",
        paint(BOLD, env!("CARGO_PKG_NAME")),
        paint(MUTED, concat!("v", env!("CARGO_PKG_VERSION")))
    );
    eprintln!();
}

/// 進行中的步驟
pub fn step(label: &str, detail: impl Display) {
    row("›", ACCENT, label, detail);
}

/// 完成的步驟
pub fn ok(label: &str, detail: impl Display) {
    row("✓", OK, label, detail);
}

/// 接續上一列的補充資訊
pub fn note(detail: impl Display) {
    eprintln!("     {:LABEL_WIDTH$}{}", "", paint(MUTED, detail));
}

/// 警告（不中斷流程）
pub fn warn(msg: impl Display) {
    eprintln!("  {}  {}", paint(WARN, "!"), paint(WARN, msg));
}

/// 致命錯誤：第一行為主訊息，其餘行（原因、提示）以淡色縮排顯示
pub fn fail(err: impl Display) {
    let text = err.to_string();
    let mut lines = text.lines();
    eprintln!();
    if let Some(first) = lines.next() {
        eprintln!("  {}  {}", paint(ERR, "✗"), paint(ERR, first));
    }
    for line in lines {
        eprintln!("     {}", paint(MUTED, line.trim_start()));
    }
    eprintln!();
}

/// 最終結果網址，寫到 stdout
pub fn link(url: &str) {
    eprintln!();
    if caps().out_color {
        println!("  {ACCENT}↗{RESET}  {BOLD}{url}{RESET}");
    } else {
        println!("{url}");
    }
}

pub fn done(elapsed: std::time::Duration) {
    eprintln!();
    eprintln!("  {}", paint(MUTED, format!("完成 · {:.2}s", elapsed.as_secs_f64())));
    eprintln!();
}

/// 粗略的終端機顯示寬度：東亞全形字元算 2 欄
fn display_width(s: &str) -> usize {
    s.chars()
        .map(|c| match c as u32 {
            0x1100..=0x115F
            | 0x2E80..=0x303E
            | 0x3041..=0x33FF
            | 0x3400..=0x4DBF
            | 0x4E00..=0x9FFF
            | 0xA000..=0xA4CF
            | 0xAC00..=0xD7A3
            | 0xF900..=0xFAFF
            | 0xFE30..=0xFE4F
            | 0xFF00..=0xFF60
            | 0xFFE0..=0xFFE6 => 2,
            _ => 1,
        })
        .sum()
}

/// Windows 10 傳統主控台預設不解析 ANSI 色碼，需要手動開啟 VT 模式。
#[cfg(windows)]
mod vt {
    use std::ffi::c_void;

    pub const STDOUT: u32 = -11i32 as u32;
    pub const STDERR: u32 = -12i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(handle: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: *mut c_void, mode: u32) -> i32;
    }

    pub fn enable(which: u32) -> bool {
        // SAFETY: 只呼叫 Win32 console API；handle 由系統回傳並先檢查有效性，
        // mode 指標指向本函式的區域變數。
        unsafe {
            let handle = GetStdHandle(which);
            if handle.is_null() || handle as isize == -1 {
                return false;
            }
            let mut mode = 0u32;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return false;
            }
            mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                || SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0
        }
    }
}

#[cfg(not(windows))]
mod vt {
    pub const STDOUT: u32 = 1;
    pub const STDERR: u32 = 2;

    pub fn enable(_: u32) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn width_counts_cjk_as_two() {
        assert_eq!(super::display_width("抓取網頁"), 8);
        assert_eq!(super::display_width("Notion"), 6);
    }
}
