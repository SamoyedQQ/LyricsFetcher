use std::fmt;

/// 全專案共用的錯誤型別，每個變體都對應一種使用者需要不同處置方式的失敗。
#[derive(Debug)]
pub enum AppError {
    /// 命令列參數錯誤
    Usage(String),
    /// 環境變數 / 設定錯誤
    Config(String),
    /// 連線、逾時、TLS 等網路層錯誤
    Network(reqwest::Error),
    /// 歌詞網站回傳非 2xx
    Http { status: u16, url: String },
    /// HTML 中找不到需要的資料
    Parse(String),
    /// Notion API 回傳錯誤
    Notion {
        status: u16,
        code: String,
        message: String,
    },
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Usage(msg) => write!(f, "參數錯誤：{msg}"),
            AppError::Config(msg) => write!(f, "設定錯誤：{msg}"),
            AppError::Network(err) => {
                write!(f, "網路錯誤：{err}")?;
                // reqwest 的錯誤訊息很精簡，把底層原因一路印出來才看得出問題
                let mut source = std::error::Error::source(err);
                while let Some(cause) = source {
                    write!(f, "\n    原因：{cause}")?;
                    source = cause.source();
                }
                Ok(())
            }
            AppError::Http { status, url } => {
                write!(f, "網頁回應 HTTP {status}：{url}")?;
                if *status == 202 {
                    write!(f, "\n    提示：此網站需要瀏覽器驗證（如 KKBOX），請改用桌面版的「瀏覽器擷取」")?;
                } else if matches!(status, 403 | 429 | 503) {
                    write!(f, "\n    提示：可能被網站的防爬機制擋下，請稍後再試或改用瀏覽器擷取")?;
                }
                Ok(())
            }
            AppError::Parse(msg) => write!(f, "解析失敗：{msg}"),
            AppError::Notion {
                status,
                code,
                message,
            } => {
                write!(f, "Notion API 錯誤（HTTP {status} / {code}）：{message}")?;
                if let Some(hint) = notion_hint(code) {
                    write!(f, "\n    提示：{hint}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for AppError {}

impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::Network(err)
    }
}

fn notion_hint(code: &str) -> Option<&'static str> {
    match code {
        "unauthorized" => Some("NOTION_API_KEY 無效，請確認 .env 中的金鑰是否完整複製"),
        "object_not_found" => Some(
            "找不到資料庫：請確認 DATABASE_ID 正確，並在資料庫頁面右上角「⋯ → 連線 (Connections)」加入你的 Integration",
        ),
        "restricted_resource" => Some("Integration 沒有此資料庫的權限，請在資料庫的 Connections 中加入它"),
        "validation_error" => Some("欄位型別或內容不符合資料庫結構，請檢查上方訊息指出的欄位"),
        "rate_limited" => Some("請求過於頻繁，請稍後再試"),
        _ => None,
    }
}

pub type Result<T> = std::result::Result<T, AppError>;
