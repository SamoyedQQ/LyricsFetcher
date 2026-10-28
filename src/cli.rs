use lyrics_fetcher::crawler::validate_url;
use lyrics_fetcher::error::{AppError, Result};
use lyrics_fetcher::notion::Tag;

pub const USAGE: &str = "\
用法：lyrics-fetcher <歌詞網址> [選項]

選項：
  --dry-run          只抓取並印出解析結果，不寫入 Notion
  --no-body          不把歌詞寫入頁面正文（只寫資料庫屬性）
  --title <歌名>     手動指定歌名（覆蓋自動解析結果）
  --artist <歌手>    手動指定歌手（覆蓋自動解析結果）
  --tag <欄位=選項>  設定 Select / Multi-select 標籤，可重複使用
  -h, --help         顯示此說明

範例：
  cargo run --release -- \"https://example.com/song/12345\"
  lyrics-fetcher \"https://...\" --tag 語言=中文 --tag 熟練度=🟠練習中";

#[derive(Debug, Default)]
pub struct Args {
    pub url: String,
    pub dry_run: bool,
    pub no_body: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub tags: Vec<Tag>,
}

pub enum Command {
    Run(Args),
    Help,
}

pub fn parse() -> Result<Command> {
    parse_from(std::env::args().skip(1))
}

fn parse_from(mut it: impl Iterator<Item = String>) -> Result<Command> {
    let mut args = Args::default();
    let mut url = None;

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--dry-run" => args.dry_run = true,
            "--no-body" => args.no_body = true,
            "--title" => args.title = Some(value_of(&mut it, "--title")?),
            "--artist" => args.artist = Some(value_of(&mut it, "--artist")?),
            "--tag" => add_tag(&mut args.tags, &value_of(&mut it, "--tag")?)?,
            s if s.starts_with('-') => return Err(AppError::Usage(format!("未知的選項 {s}"))),
            _ if url.is_some() => return Err(AppError::Usage("只能指定一個網址".into())),
            _ => url = Some(arg),
        }
    }

    let url = url.ok_or_else(|| AppError::Usage("缺少歌詞網址".into()))?;
    validate_url(&url)?;
    args.url = url;
    Ok(Command::Run(args))
}

fn value_of(it: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    it.next()
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| AppError::Usage(format!("{flag} 後面需要接一個值")))
}

/// `欄位=選項`；同一欄位重複指定時合併（Multi-select 用）
fn add_tag(tags: &mut Vec<Tag>, spec: &str) -> Result<()> {
    let (prop, value) = spec
        .split_once('=')
        .map(|(p, v)| (p.trim(), v.trim()))
        .filter(|(p, v)| !p.is_empty() && !v.is_empty())
        .ok_or_else(|| AppError::Usage(format!("--tag 格式應為 欄位=選項，收到：{spec}")))?;
    match tags.iter_mut().find(|t| t.property == prop) {
        Some(t) => t.values.push(value.to_string()),
        None => tags.push(Tag { property: prop.to_string(), values: vec![value.to_string()] }),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_vec(v: &[&str]) -> Result<Command> {
        parse_from(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_url_and_flags() {
        let Command::Run(a) = parse_vec(&["https://a.b/c", "--dry-run", "--artist", "X"]).unwrap() else {
            panic!("expected Run");
        };
        assert_eq!(a.url, "https://a.b/c");
        assert!(a.dry_run);
        assert_eq!(a.artist.as_deref(), Some("X"));
    }

    #[test]
    fn parses_tags() {
        let Command::Run(a) = parse_vec(&["https://a.b", "--tag", "語言=中文", "--tag", "分類=a", "--tag", "分類 = b"]).unwrap()
        else {
            panic!("expected Run");
        };
        assert_eq!(a.tags.len(), 2);
        assert_eq!(a.tags[1].values, ["a", "b"]);
        assert!(parse_vec(&["https://a.b", "--tag", "語言"]).is_err());
        assert!(parse_vec(&["https://a.b", "--tag", "=x"]).is_err());
    }

    #[test]
    fn rejects_missing_or_bad_url() {
        assert!(parse_vec(&[]).is_err());
        assert!(parse_vec(&["example.com"]).is_err());
        assert!(parse_vec(&["https://a.b", "--title"]).is_err());
    }
}
