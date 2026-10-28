mod cli;
mod ui;

use std::process::ExitCode;
use std::time::Instant;

use cli::Command;
use lyrics_fetcher::genius;
use lyrics_fetcher::crawler::{self, SelectorOverrides};
use lyrics_fetcher::error::{AppError, Result};
use lyrics_fetcher::notion::{Notion, NotionConfig, PageOptions};

// 單次執行的 CLI 只有少量 I/O，單執行緒 runtime 啟動更快、資源更少
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ui::fail(&e);
            if matches!(e, AppError::Usage(_)) {
                eprintln!("{}\n", cli::USAGE);
            }
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let args = match cli::parse()? {
        Command::Help => {
            println!("{}", cli::USAGE);
            return Ok(());
        }
        Command::Run(args) => args,
    };

    // 沒有 .env 不算錯誤（可直接使用系統環境變數），但檔案格式錯誤要回報
    if let Err(e) = dotenvy::dotenv()
        && !e.not_found()
    {
        return Err(AppError::Config(format!(".env 讀取失敗：{e}")));
    }

    let started = Instant::now();
    ui::header();

    let client = crawler::build_client()?;
    // 先驗證設定再發請求，設定錯誤時不浪費網路往返
    let selectors = SelectorOverrides::from_env()?;
    let notion = if args.dry_run {
        None
    } else {
        Some(Notion::new(client.clone(), &NotionConfig::from_env()?)?)
    };

    ui::step("抓取網頁", &args.url);
    // Genius 會一併抓英文翻譯頁並合併成中英對照
    let fetch = genius::fetch_any(&client, &args.url, &selectors);
    let (song, schema) = match &notion {
        Some(n) => {
            ui::step("讀取資料庫", "Notion schema");
            // 爬蟲與讀取 schema 互不相依，並行執行
            let (song, schema) = tokio::join!(fetch, n.fetch_schema());
            (song?, Some(schema?))
        }
        None => (fetch.await?, None),
    };

    let genius::Fetched { mut song, bilingual, note } = song;
    if let Some(t) = args.title {
        song.title = t;
    }
    if let Some(a) = args.artist {
        song.artist = a;
    }
    if song.title.trim().is_empty() {
        return Err(AppError::Parse(
            "找不到歌名。\n可用 --title 手動指定，或在 .env 設定 TITLE_SELECTOR".into(),
        ));
    }

    let artist = if song.artist.is_empty() { "（未知歌手）" } else { &song.artist };
    ui::ok("解析完成", format!("{} — {artist}", song.title));
    ui::note(format!(
        "{} 行 · {} 字",
        song.lyrics.lines().filter(|l| !l.trim().is_empty()).count(),
        song.lyrics.chars().filter(|c| !c.is_whitespace()).count()
    ));
    if song.artist.is_empty() {
        ui::warn("未解析到歌手，可用 --artist 指定");
    }
    // 有英譯時改成「一行原文、一行英譯」，正文以中英對照格式寫入
    if let Some(n) = note {
        ui::note(n);
    }
    let bilingual = match bilingual {
        Some(text) => {
            song.lyrics = text;
            ui::note("使用中英對照格式");
            true
        }
        None => false,
    };

    let (Some(notion), Some(schema)) = (notion, schema) else {
        // dry-run：歌詞輸出到 stdout，可導向檔案
        eprintln!();
        println!("{}", song.lyrics);
        ui::done(started.elapsed());
        return Ok(());
    };

    ui::ok("欄位對應", schema.summary());
    schema.warnings.iter().for_each(ui::warn);
    ui::step("寫入 Notion", "建立頁面中");
    let opts = PageOptions {
        tags: args.tags,
        texts: Vec::new(),
        bilingual,
        write_body: !args.no_body,
    };
    let created = notion.create_page(&schema, &song, &opts).await?;
    ui::ok("成功寫入 Notion", format!("{} 個段落區塊", created.blocks));
    created.warnings.iter().for_each(ui::warn);

    ui::link(&created.url);
    ui::done(started.elapsed());
    Ok(())
}
