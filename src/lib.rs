//! 核心函式庫：抓取歌詞與寫入 Notion。CLI（src/main.rs）與 GUI（src-tauri）共用。

pub mod bilingual;
pub mod crawler;
pub mod error;
pub mod genius;
pub mod notion;
pub mod search;
pub mod zh;

pub use reqwest::{Client as HttpClient, Url};
