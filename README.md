# Lyrics Fetcher

搜尋歌名或貼上網址，把歌詞（可含中英對照）、YouTube 伴奏連結與標籤一鍵匯入 Notion 資料庫。以 Rust 撰寫，提供桌面版（Tauri）與 CLI 兩種介面。

- 同時搜尋 KKBOX、Asianpopweekly、Genius、LyricsTranslate 與 YouTube 伴奏
- 自動產生「一行原文、一行英譯」的中英對照；可任選兩個版本自動合併
- 自動轉成台灣繁體、依歌詞自動選語言標籤
- API Key 存在系統認證管理員，不寫入任何檔案

## 下載與安裝（Windows）

1. 到 [Releases](../../releases/latest) 下載 `LyricsFetcher-*-windows-x64.zip`
2. 解壓縮到任意資料夾，執行 `lyrics-fetcher-gui.exe`
3. 第一次執行時 Windows 可能顯示「Windows 已保護您的電腦」（程式沒有數位簽章）：點「其他資訊」→「仍要執行」

需要 Microsoft Edge WebView2 Runtime：Windows 11 已內建，Windows 10 通常也已隨 Edge 安裝；若開啟後畫面空白，請到 <https://developer.microsoft.com/microsoft-edge/webview2/> 下載「Evergreen Bootstrapper」安裝。

zip 內也附 CLI 版 `lyrics-fetcher.exe`，用法見下方「CLI」。

## 使用教學

### 第一次設定

1. 依下方「[建立 Notion Integration](#2-建立-notion-integration)」與「[準備資料庫並授權](#3-準備資料庫並授權)」建立 Integration、把資料庫連線到 Integration
2. 開啟程式，點右上角的設定圖示，填入 **API Key**（Internal Integration Secret）與 **Database ID**（可直接貼資料庫網址）
3. 按「測試連線」，確認歌名、歌手、歌詞等欄位對應正確

### 搜尋並匯入一首歌

1. 在上方輸入歌名（可加歌手，例如「晴天 周杰倫」），按 `Enter`
2. 左側「歌詞」清單點一個結果，右側會顯示預覽；歌名、歌手、歌詞都可以直接修改
3. 「伴奏」清單點一部影片選定伴奏（↗ 可先在 YouTube 試聽）
4. 確認語言、熟練度等標籤後，按「匯入 Notion」（或 `Ctrl` + `Enter`）

### 合併中英對照

清單中標「中英」的結果已附英文翻譯，點選後會自動使用中英格式。沒有的話：

1. 按一個原文歌詞（例如 KKBOX）右側的「＋」
2. 再按一個英文翻譯（例如 LyricsTranslate）的「＋」
3. 程式會自動判斷哪個是原文、哪個是英譯，逐段對齊合併。選到兩個同語言的版本（或其中一個已是中英對照）時會自動去重

「在瀏覽器搜尋 LyricsTranslate 的英文翻譯」也可以按「＋」：抓取時會開啟視窗，點選英文翻譯頁後自動擷取。

### 其他操作

| 操作 | 說明 |
|---|---|
| 切換到「貼上網址」 | 貼上歌詞頁網址按 `Enter` 預覽；`Ctrl` + `Enter` 直接匯入 |
| 拖放連結 | 把瀏覽器網址列的連結拖進視窗，會自動預覽 |
| 手動輸入 | 直接貼上歌詞；匯入時會自動整理空行、移除作詞作曲資訊 |
| 自動轉繁 | 抓到的歌名、歌手、歌詞自動轉為台灣繁體（日文、韓文不轉） |
| 中英格式 | 開啟時以「原文 Heading 2 + 橘色粗體英譯」寫入頁面正文 |
| 寫入正文 | 關閉時只寫資料庫欄位，不寫頁面內容 |

## 桌面版細節

```bash
# 需要先安裝 Tauri CLI：cargo install tauri-cli --version "^2"
cargo tauri dev                 # 開發模式（請在 src-tauri/ 目錄下執行）
cargo tauri build               # 產出安裝檔：target/release/bundle/{nsis,msi}/
```

第一次開啟時，點右上角的設定圖示，填入 **API Key** 與 **Database ID**（取得方式請見下方第 2、3 節），再按「測試連線」確認欄位對應。

網址列上方可以切換兩種輸入方式：**搜尋歌名**、**貼上網址**。

| 操作 | 說明 |
|---|---|
| `Enter`（搜尋歌名） | 同時搜尋 KKBOX、Asianpopweekly、Genius 的歌詞，以及 YouTube 上的伴奏影片 |
| `Enter`（貼上網址） | 抓取並預覽，歌名、歌手、歌詞都可以直接修改 |
| `Ctrl` + `Enter` / 「匯入」 | 一鍵完成抓取與匯入；如果已有預覽，會保留你修改過的內容 |
| 拖放連結 | 把瀏覽器網址列的連結拖進視窗，會自動預覽 |

### 搜尋歌名

搜尋結果分成兩個清單：

- **歌詞**：列出 KKBOX、Asianpopweekly、Genius、LyricsTranslate 找到的版本，標示來源網站；有中英對照的版本標上「中英」並**排在最前面**。點一下即載入預覽，按「＋」任選兩個可合併成中英對照
- **LyricsTranslate**：網站有 Cloudflare 驗證，搜尋改用 DuckDuckGo 的 `site:` 查詢，頁面則跟 KKBOX 一樣由擷取視窗載入。英文翻譯頁的原文與譯文是左右逐行對照，會直接配對成中英格式。DuckDuckGo 查詢太頻繁時會暫時拒絕自動搜尋，這時可按清單最下方的「在瀏覽器搜尋 LyricsTranslate 的英文翻譯」，在開啟的視窗中自己點選翻譯頁，載入後會自動擷取
- **伴奏**：列出 YouTube 影片的標題、頻道與長度，點一下就選定（再點一次取消），↗ 可以先在 YouTube 試聽。載入歌詞後，可按「以歌名＋歌手重新搜尋」讓結果更準確

選好的伴奏會填入資料庫中名稱含「伴奏」的 Text 欄位：文字依影片標題猜測為「純伴奏 / 和聲伴奏 / 翻唱伴奏」，並連結到該影片，跟資料庫既有的格式相同。其他 Text 欄位（例如「備註」）也會出現在預覽卡片中，並提供資料庫中常用的值（例如「原KEY」）作為建議。

### 簡繁轉換與排序

- 預覽卡片下方的「自動轉繁」開啟時，抓到的歌名、歌手與歌詞會自動轉為台灣繁體（使用 MediaWiki 字詞表）；偵測為日文或韓文的歌詞不轉換
- 搜尋結果依相符程度分段：**歌名相符** → **簡繁轉換後相符**（例如搜「當你」找到簡體的「当你」）→ **部分相符** → **其他結果**；同一段中有中英對照的排前面

### 中英格式

預覽卡片下方的「中英格式」開關，對應資料庫既有的雙語頁面格式：

- 原文為白色的 Heading 2，英譯為下一行的橘色粗體段落
- `<Verse 1>`、`[Chorus]` 這類段落標記寫成粗體段落，段落之間空一行

**Genius**：選 Genius 的原文頁時，會自動到該曲的「Genius English Translations」翻譯頁抓英文，逐段合併，段落標記也改用英文名稱（`<Verse 1>`、`<Chorus>`）。翻譯的斷行常與原文不同（一行拆成兩行或合併），這種段落會依文字長度對齊，預覽上方會提示需要檢查；沒有英文翻譯的歌曲則只匯入原文。搜尋清單中有英文翻譯的 Genius 歌曲會標上「中英」。

其他來源附英譯時也會自動開啟，例如 Asianpopweekly 的「寶貝 今晚的我有點醉 Babe, I'm a little tipsy tonight」會拆成兩行，拼音行則會移除。預覽中看到的就是「一行原文、一行英譯」的文字，可以直接修改；關閉開關會還原成原本的歌詞（前提是還沒修改過）。CLI 偵測到英譯時也會自動使用這個格式。

### KKBOX 與需要瀏覽器驗證的網站

KKBOX 前面有 AWS WAF 人機驗證，一般 HTTP 請求只會拿到驗證頁，所以 CLI 無法使用。桌面版的處理方式：

- **KKBOX 搜尋**：一般請求被 WAF 擋下（回應 `202` + `x-amzn-waf-action: challenge`）時，改在隱藏的內嵌瀏覽器載入 KKBOX 首頁，讓驗證自動完成後再呼叫站內搜尋 API；超過 12 秒仍未通過會跳出視窗讓你完成驗證
- **KKBOX 網址**：自動開啟擷取視窗（WebView2，跟一般瀏覽器一樣），等 `.track-content .lyrics` 載入完成後擷取，完成後視窗自動關閉。若網站要求人機驗證，請在該視窗中完成
- **其他網站**：抓取失敗時，錯誤訊息下方會出現「用瀏覽器擷取」與「手動輸入」兩個按鈕
- **手動輸入**：網址列右下的「手動輸入」可以直接貼上歌詞；匯入時會自動整理空行，並移除開頭的作詞、作曲資訊

### 標籤

資料庫中除了歌手以外的 **Select / Multi-select** 欄位（例如「語言」、「熟練度」）會顯示在預覽卡片的歌手下方，選項和顏色都直接讀自 Notion。Select 只能選一個，再點一次可以取消；Multi-select 可以複選。

- 語言欄位會依歌詞文字自動選好（中文、日本語、한국어、English 等名稱都能辨識），手動點過之後就不再自動變更
- 只能選資料庫中已有的選項，不會自動在 Notion 新增選項；要加選項，請先在 Notion 中新增，再到設定按「測試連線」重新讀取
- 某個標籤沒選時，匯入後會提醒你：Notion 檢視如果依該欄位篩選，頁面就不會出現在那個檢視

擷取視窗沒有任何應用程式權限，資料透過攔截一個保留網域（`.invalid`）的導向傳回，不會經過網路。

- API Key 存放在**系統認證管理員**（Windows Credential Manager / macOS Keychain），不會寫入任何檔案，也不會傳回前端
- 其他設定存放在應用程式設定資料夾的 `settings.json`
- 抓取網頁時會同時預先讀取 Notion 資料庫結構並快取，按下匯入時就不必再等待

## 功能

- **模擬 Chrome 請求**：送出 Chrome 的 User-Agent 與 `Accept`、`Sec-Fetch-*` 等標頭，可以通過基本的防爬檢查
- **自動轉碼**：依 HTTP 標頭或 `<meta charset>` 判斷編碼，Big5 等舊網站也能正確讀取
- **歌詞解析**：保留換行與段落，自動移除網站浮水印與 LRC 時間軸
- **內建網站規則**：Genius、Asianpopweekly、KKBOX、魔鏡歌詞網 (Mojim)、AZLyrics；其他網站使用通用規則，抓不到時可以自訂 CSS selector
- **自動對應欄位**：先讀取資料庫結構，再依各欄位的實際型別（Text / Select / Multi-select / URL）組出正確的資料
- **處理 Notion 長度限制**：歌詞自動切成每塊不超過 2000 字元，並盡量在換行處切開；區塊超過 100 個時分批寫入
- **並行請求**：抓取網頁與讀取 Notion 資料庫結構同時進行

## 從原始碼編譯

一般使用者直接下載 Releases 的 zip 即可，以下為開發者說明。

```bash
cargo build --release --workspace
# 產出：target/release/lyrics-fetcher-gui.exe（桌面版）、target/release/lyrics-fetcher.exe（CLI）
```

## 1. 安裝 Rust

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Windows：安裝 https://rustup.rs 提供的 rustup-init.exe（需要 Visual Studio C++ Build Tools）
```

需要 Rust 1.88 以上版本（本專案使用 edition 2024）。

## 2. 建立 Notion Integration

1. 前往 <https://www.notion.so/profile/integrations>，點 **New integration**
2. 類型選 **Internal**，選擇你的工作區，名稱自訂（例如 `Lyrics Fetcher`）
3. 在 **Capabilities** 勾選：
   - ✅ Read content（讀取資料庫結構）
   - ✅ Insert content（建立頁面）
   - 其餘權限都不需要，依最小權限原則不必勾選
4. 儲存後，複製 **Internal Integration Secret**

## 3. 準備資料庫並授權

### 建議的欄位

| 欄位名稱 | 型別 | 說明 |
|---|---|---|
| `Name` | Title | 歌名（必有；資料庫的標題欄位叫什麼名字都可以） |
| `Artist` | Text、Select 或 Multi-select | 歌手；Multi-select 會把「A & B feat. C」拆成多個選項 |
| `Lyrics` | Text | 完整歌詞（選用；屬性最多可存 200,000 字元） |
| `URL` | URL | 來源網址（選用） |

欄位名稱可以用中文，`歌手`、`歌詞`、`來源`、`網址` 等常見名稱都會自動辨識；也可以在 `.env` 用 `NOTION_*_PROPERTY` 指定。資料庫中沒有的欄位會直接略過，不會報錯。

不論有沒有 `Lyrics` 欄位，歌詞都會依段落寫入頁面正文。如果不想寫入正文，執行時加上 `--no-body`。

### 授權 Integration 存取資料庫（最常漏掉的步驟）

打開資料庫頁面 → 右上角 **⋯** → **Connections**（連線）→ 搜尋並加入剛建立的 Integration。

沒做這一步的話，執行時會出現 `object_not_found` 錯誤。

### 取得 Database ID

用瀏覽器打開資料庫（要開資料庫本身，不是內嵌了資料庫的頁面），網址格式如下：

```
https://www.notion.so/<workspace>/<32碼ID>?v=<view ID>
                                  ^^^^^^^^ 這段就是 DATABASE_ID
```

也可以直接貼整個網址到 `DATABASE_ID`，程式會自動取出 ID（`?v=` 後面的 view ID 會被忽略）。

## 4. 設定環境變數（CLI）

```bash
cp .env.example .env
```

編輯 `.env`，填入 `NOTION_API_KEY` 與 `DATABASE_ID`。`.env` 已列入 `.gitignore`，不會被提交到版本控制。

## 5. 從原始碼編譯（CLI）

```bash
# 開發時直接執行
cargo run --release -- "https://example.com/song/12345"

# 編譯最佳化的執行檔（啟用 LTO、單一 codegen unit、panic=abort、strip）
cargo build --release
# 產出：target/release/lyrics-fetcher(.exe)，可放到 PATH 中任何位置使用
./target/release/lyrics-fetcher "https://example.com/song/12345"
```

### 選項

| 選項 | 說明 |
|---|---|
| `--dry-run` | 只抓取與解析，不寫入 Notion，歌詞輸出到 stdout；用來確認解析結果，執行時不需要 Notion 金鑰 |
| `--no-body` | 只寫入資料庫欄位，不寫頁面正文 |
| `--title <歌名>` | 手動指定歌名，覆蓋自動解析的結果 |
| `--artist <歌手>` | 手動指定歌手，覆蓋自動解析的結果 |
| `--tag <欄位=選項>` | 設定 Select / Multi-select 標籤，例如 `--tag 語言=中文`；同一欄位重複指定即為複選 |
| `-h`, `--help` | 顯示說明 |

```bash
# 先確認解析結果
lyrics-fetcher "https://..." --dry-run

# 把歌詞存成檔案（進度訊息寫到 stderr，不會混進檔案）
lyrics-fetcher "https://..." --dry-run > lyrics.txt
```

## 遇到不支援的網站

1. 先用 `--dry-run` 測試。如果出現「找不到歌詞區塊」：
2. 在瀏覽器中對歌詞按右鍵 → **檢查**，找出包住歌詞的元素
3. 在 `.env` 加上對應的 CSS selector，例如：
   ```
   LYRICS_SELECTOR=div.song-lyrics
   TITLE_SELECTOR=h1
   ARTIST_SELECTOR=.artist-name
   ```

如果網站是用 JavaScript 動態載入歌詞，HTML 原始碼裡本來就沒有歌詞，本工具無法抓取（判斷方式：在瀏覽器按 `Ctrl+U` 檢視原始碼，搜尋歌詞中的一句）。

## 常見錯誤

| 訊息 | 原因與解法 |
|---|---|
| `unauthorized` | `NOTION_API_KEY` 錯誤或沒有完整複製 |
| `object_not_found` | `DATABASE_ID` 錯誤，或資料庫還沒加入 Integration 的 Connections |
| `validation_error` | 欄位型別不符，請依錯誤訊息檢查對應的欄位 |
| `HTTP 403 / 429` | 被網站的防爬機制擋下，請稍後再試 |
| 匯入成功但在 Notion 找不到 | 先確認設定中的 Database ID 是你正在看的資料庫（複製出來的副本 ID 會不同）；再檢查檢視是否依標籤篩選，沒填標籤的頁面不會出現在那些檢視 |
| 顏色顯示為亂碼 | 設定環境變數 `NO_COLOR=1` 關閉顏色 |

## 安全性設計

- API 金鑰只從環境變數讀取，送出時標記為敏感標頭，不會出現在任何日誌或錯誤訊息中
- `DATABASE_ID` 與 API 回傳的頁面 ID 都會先驗證為十六進位 ID，才放進請求路徑，避免路徑注入
- 網頁下載上限為 8 MiB，以串流方式讀取並檢查大小，避免異常網頁耗盡記憶體
- HTML 走訪使用明確堆疊而不是遞迴，惡意的深層巢狀 HTML 不會造成堆疊溢位
- 只有 Notion 明確回應 429（表示請求未被處理）時才重試建立頁面；5xx 錯誤只對讀取請求重試，避免重複建立頁面
- 最多跟隨 5 次轉址；連線逾時 10 秒，整體請求逾時 30 秒

## 專案結構

```
src/                 核心函式庫 + CLI
├── lib.rs           函式庫入口（CLI 與 GUI 共用）
├── crawler.rs       HTTP 請求、轉碼、HTML 解析、歌詞清理
├── search.rs        搜尋歌詞網（KKBOX、Asianpopweekly、Genius、LyricsTranslate）與 YouTube 伴奏
├── bilingual.rs     中英對照的偵測、轉換、逐行分類，與任選兩份歌詞的合併
├── genius.rs        Genius 原文頁與英文翻譯頁的合併
├── zh.rs            簡繁轉換
├── notion.rs        Notion API：schema 對應、rich text 切塊、分批寫入
├── error.rs         錯誤型別與提示訊息
├── main.rs          CLI 流程控制
├── cli.rs           命令列參數解析
└── ui.rs            終端機介面（暗色系配色、Windows VT 支援）
src-tauri/           桌面版後端
├── src/main.rs      Tauri 指令：搜尋、抓取、匯入、設定、開啟頁面
├── src/settings.rs  設定檔與系統認證管理員
├── src/*.js         注入擷取 / 搜尋視窗的腳本（KKBOX、LyricsTranslate）
├── tauri.conf.json  視窗與 CSP 設定
└── capabilities/    前端權限（僅 core:default）
ui/                  桌面版前端（純 HTML/CSS/JS，無建置步驟）
```

執行測試：`cargo test --workspace`

### 桌面版的安全設計

- **CSP**：只允許載入應用程式本身的資源，禁止 inline script、外部連線與 iframe
- **前端寫入**：所有網頁內容都以 `textContent` 寫入畫面，不使用 `innerHTML`
- **前端權限**：只有 `core:default`。開啟網址由後端驗證，只允許 `https://*.notion.so`、`*.notion.site`、`*.notion.com` 與 YouTube
- **輸入檢查**：前端送來的歌詞上限 1 MiB，網址與欄位連結只接受 http(s)，selector 會先驗證語法
- **搜尋**：查詢字串一律經過 URL 編碼；回應有大小上限；YouTube 影片 ID 只接受英數字與 `-_`

## 注意事項

本工具供個人整理歌詞使用。歌詞受著作權保護，請勿公開散布抓取的內容，並遵守各網站的使用條款與 robots.txt。
