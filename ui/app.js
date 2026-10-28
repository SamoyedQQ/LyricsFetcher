// 所有動態內容一律以 textContent 寫入，不使用 innerHTML，避免網頁內容造成 XSS
"use strict";

const { invoke } = window.__TAURI__.core;
const $ = (id) => document.getElementById(id);

const HISTORY_KEY = "lf.history";
const MODE_KEY = "lf.mode";
const AUTO_HANT_KEY = "lf.autoHant";
const HISTORY_MAX = 12;
/** 有人機驗證、一般請求抓不到的網站：直接改用內嵌瀏覽器擷取 */
const BROWSER_HOSTS = ["kkbox.com", "lyricstranslate.com"];
/** Notion 選項顏色（與 style.css 的 .c-* 對應） */
const NOTION_COLORS = ["default", "gray", "brown", "orange", "yellow", "green", "blue", "purple", "pink", "red"];
/** 依歌詞文字判斷語言後，比對選項名稱 */
const LANGUAGE_OPTIONS = {
  ko: /한국어|韓文|韓語|韩语|korean/i,
  ja: /日本語|日文|日語|日语|japanese/i,
  zh: /中文|華語|华语|國語|国语|chinese|mandarin/i,
  en: /english|英文|英語|英语/i,
};
/** 名稱符合的 Text 欄位填入 YouTube 伴奏（文字為類型、連結為影片） */
const ACCOMPANIMENT_FIELD = /伴奏|accompan|instrumental|karaoke/i;
const ACCOMPANIMENT_LABELS = ["純伴奏", "和聲伴奏", "翻唱伴奏"];
/** 搜尋結果的相符程度（對應後端 relevance 0 / 1 / 2 / 3） */
const RELEVANCE_LABELS = ["歌名相符", "簡繁轉換後相符", "部分相符", "其他結果"];
/** 與 style.css 的雙欄斷點一致：寬視窗時預覽固定在右側，歌詞框自行捲動 */
const WIDE = window.matchMedia("(min-width: 980px)");

const el = {
  modeSearch: $("mode-search"), modeUrl: $("mode-url"),
  urlForm: $("url-form"), url: $("url"), previewBtn: $("preview-btn"), quickImport: $("quick-import"),
  banner: $("setup-banner"), status: $("status"), statusText: $("status-text"), alert: $("alert"),
  result: $("result"), resultText: $("result-text"), resultOpen: $("result-open"), resultWarnings: $("result-warnings"),
  search: $("search"), lyricsHits: $("lyrics-hits"), lyricsNote: $("lyrics-note"),
  videoHits: $("video-hits"), videoNote: $("video-note"), videoResearch: $("video-research"),
  preview: $("preview"), previewEmpty: $("preview-empty"), previewNote: $("preview-note"), title: $("title"), artist: $("artist"), lyrics: $("lyrics"),
  stats: $("stats"), writeBody: $("write-body"), bilingual: $("bilingual"), autoHant: $("auto-hant"), importBtn: $("import-btn"),
  tags: $("tags"), fields: $("fields"),
  historyWrap: $("history-wrap"), history: $("history"), historyClear: $("history-clear"),
  sheet: $("settings"), settingsForm: $("settings-form"), settingsMsg: $("settings-msg"),
  keyState: $("key-state"), clearKey: $("clear-key"), testConn: $("test-conn"),
  s: {
    apiKey: $("s-key"), databaseId: $("s-db"),
    artistProperty: $("s-artist-prop"), lyricsProperty: $("s-lyrics-prop"), urlProperty: $("s-url-prop"),
    titleSelector: $("s-title-sel"), artistSelector: $("s-artist-sel"), lyricsSelector: $("s-lyrics-sel"),
  },
};

const state = {
  settings: null,
  /** "search"：搜尋歌名；"url"：貼上網址 */
  mode: "search",
  /** 兩種模式各自保留輸入框內容 */
  inputs: { search: "", url: "" },
  /** 目前預覽對應的來源網址；網址列改變後預覽即視為過期 */
  previewUrl: null,
  busy: false,
  lastPageUrl: null,
  /** 資料庫的標籤欄位；null = 尚未讀取 */
  tagFields: null,
  /** 欄位名稱 → 已選選項（Set） */
  selected: new Map(),
  /** 使用者手動點過的欄位，不再自動判斷 */
  touched: new Set(),
  /** 可填寫的 Text 欄位名稱；null = 尚未讀取 */
  textFields: null,
  /** 欄位名稱 → { text, link, note }（note 為選中影片的標題，只供顯示） */
  texts: new Map(),
  /** 使用者手動輸入過文字的欄位，選影片時不覆蓋 */
  touchedTexts: new Set(),
  /** 欄位名稱 → 資料庫中常用的值 */
  suggestions: new Map(),
  suggestionsLoaded: false,
  /** 搜尋結果中選中的歌詞網址與伴奏影片 */
  lyricsHit: null,
  /** 按「＋」選取、要合併成中英對照的歌詞（最多兩個，依點選順序） */
  mergePicks: [],
  /** 網址 → 抓到的歌詞，合併時不必重抓（LyricsTranslate 擷取很慢） */
  songCache: new Map(),
  video: null,
  lastLyrics: null,
  lastVideos: null,
  /** 轉成中英格式前的歌詞；關閉中英格式且未修改時還原 */
  plainLyrics: null,
  bilingualText: null,
};

// ─── 共用 ────────────────────────────────────────────

function setBusy(busy, text = "") {
  state.busy = busy;
  el.status.hidden = !busy;
  el.statusText.textContent = text;
  for (const b of [el.previewBtn, el.quickImport, el.importBtn, el.videoResearch]) b.disabled = busy;
}

/** @param {{label: string, run: () => void}[]} actions 錯誤訊息下方的備案按鈕 */
function showError(message, actions = []) {
  const [first, ...rest] = String(message).split("\n");
  el.alert.replaceChildren();
  const strong = document.createElement("strong");
  strong.textContent = first;
  el.alert.append(strong);
  for (const line of rest.map((l) => l.trim()).filter(Boolean)) {
    const p = document.createElement("p");
    p.textContent = line;
    el.alert.append(p);
  }
  if (actions.length) {
    const row = Object.assign(document.createElement("div"), { className: "alert-actions" });
    for (const a of actions) {
      const b = Object.assign(document.createElement("button"), { type: "button", className: "btn ghost", textContent: a.label });
      b.addEventListener("click", a.run);
      row.append(b);
    }
    el.alert.append(row);
  }
  el.alert.hidden = false;
}

function make(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function hostOf(url) {
  try {
    return new URL(url).hostname.toLowerCase();
  } catch {
    return "";
  }
}

function needsBrowser(url) {
  const host = hostOf(url);
  return BROWSER_HOSTS.some((h) => host === h || host.endsWith(`.${h}`));
}

function clearMessages() {
  el.alert.hidden = true;
  el.result.hidden = true;
}

function isConfigured() {
  return Boolean(state.settings?.hasApiKey && state.settings?.databaseId);
}

function updateStats() {
  const text = el.lyrics.value;
  const lines = text.split("\n").filter((l) => l.trim()).length;
  const chars = [...text.replace(/\s/g, "")].length;
  el.stats.textContent = `${lines} 行 · ${chars.toLocaleString()} 字`;
}

function autosize() {
  if (WIDE.matches) {
    el.lyrics.style.height = "";
    return;
  }
  el.lyrics.style.height = "auto";
  el.lyrics.style.height = `${el.lyrics.scrollHeight}px`;
}

// ─── 輸入方式 ────────────────────────────────────────

function setMode(mode) {
  if (state.mode !== mode) {
    state.inputs[state.mode] = el.url.value;
    el.url.value = state.inputs[mode];
  }
  state.mode = mode;
  const search = mode === "search";
  document.body.dataset.mode = mode;
  el.modeSearch.setAttribute("aria-checked", String(search));
  el.modeUrl.setAttribute("aria-checked", String(!search));
  el.url.type = search ? "search" : "url";
  el.url.placeholder = search ? "歌名或歌手，例如：愛你但說不出口" : "https://";
  el.url.setAttribute("aria-label", search ? "歌名" : "歌詞網址");
  el.previewBtn.textContent = search ? "搜尋" : "預覽";
  el.search.hidden = !search || !state.lastLyrics;
  try { localStorage.setItem(MODE_KEY, mode); } catch { /* 只是不記住模式 */ }
}

// ─── 搜尋 ────────────────────────────────────────────

/** 依影片標題猜伴奏類型 */
function guessAccompaniment(title) {
  if (/和聲|和声|harmony|backing/i.test(title)) return "和聲伴奏";
  if (/翻唱|cover/i.test(title)) return "翻唱伴奏";
  return "純伴奏";
}

async function runSearch() {
  const query = el.url.value.trim();
  if (!query) return el.url.focus();
  clearMessages();
  setBusy(true, "正在搜尋歌詞網與 YouTube 伴奏…");
  state.lastQuery = query;
  state.lyricsHit = null;
  state.mergePicks = [];
  state.songCache = new Map();
  state.video = null;
  // LyricsTranslate（以及被擋時的 KKBOX）在隱藏的內嵌瀏覽器中搜尋，較慢，結果到了再併入清單
  state.browserHits = { LyricsTranslate: { status: "pending", hits: [], error: "" } };
  searchInBrowser("LyricsTranslate", "search_lyricstranslate", query);
  try {
    const [lyrics, videos] = await Promise.allSettled([
      invoke("search_lyrics", { query }),
      invoke("search_videos", { query: `${query} 伴奏` }),
    ]);
    state.lastLyrics = lyrics;
    state.lastVideos = videos;
    // KKBOX 的 AWS WAF 擋下一般請求：改在內嵌瀏覽器中搜尋
    if (lyrics.status === "fulfilled" && lyrics.value.kkboxBlocked) {
      state.browserHits.KKBOX = { status: "pending", hits: [], error: "" };
      searchInBrowser("KKBOX", "search_kkbox", query);
    }
    renderLyricsHits();
    renderVideoHits();
    el.search.hidden = false;
    applyVideo();
    renderFields();
  } finally {
    setBusy(false);
  }
}

async function searchInBrowser(site, command, query) {
  let result;
  try {
    result = { status: "done", hits: await invoke(command, { query }), error: "" };
  } catch (e) {
    result = { status: "error", hits: [], error: String(e) };
  }
  if (state.lastQuery !== query) return; // 已有新的搜尋
  state.browserHits[site] = result;
  renderLyricsHits();
}

/** 預覽載入後，以「歌名 歌手 伴奏」重新搜尋，結果通常更準 */
async function researchVideos() {
  // 「愛你但說不出口 (Hard to Say)」→「愛你但說不出口」
  const title = el.title.value.replace(/\s*[(（][^)）]*[)）]/g, " ").trim();
  const query = [title, el.artist.value.trim(), "伴奏"].filter(Boolean).join(" ");
  if (!title || state.busy) return;
  setBusy(true, "正在重新搜尋伴奏…");
  try {
    state.lastVideos = await Promise.allSettled([invoke("search_videos", { query })]).then(([r]) => r);
    renderVideoHits();
  } finally {
    setBusy(false);
  }
}

function hitRow(button, extra = []) {
  const li = document.createElement("li");
  li.append(button, ...extra);
  return li;
}

function renderLyricsHits() {
  const res = state.lastLyrics;
  if (!res) return;
  if (res.status === "rejected") {
    el.lyricsNote.textContent = "";
    el.lyricsHits.replaceChildren(make("li", "hits-empty", String(res.reason)));
    return;
  }
  const extra = Object.entries(state.browserHits || {});
  // 併入內嵌瀏覽器的結果後重新排序：相符程度優先，同級中英對照在前（sort 為穩定排序）
  const hits = [...res.value.hits, ...extra.flatMap(([, r]) => r.hits)]
    .sort((a, b) => a.relevance - b.relevance || b.translated - a.translated);
  const warnings = [...res.value.warnings];
  for (const [site, r] of extra) if (r.status === "error") warnings.push(`${site}：${r.error}`);
  const parts = [`${hits.length} 筆`];
  const pending = extra.filter(([, r]) => r.status === "pending").map(([site]) => site);
  if (pending.length) parts.push(`${pending.join("、")} 搜尋中…`);
  if (warnings.length) parts.push(`${warnings.map((w) => w.split("：")[0]).join("、")} 無法連線`);
  if (state.mergePicks.length === 1) parts.push("已選 1 個，再按一個「＋」合併中英");
  el.lyricsNote.textContent = parts.join(" · ");
  el.lyricsNote.title = warnings.join("\n");
  const rows = [];
  let tier = null;
  for (const hit of hits) {
    // 依相符程度分段，段落之間加上標題線
    if (hit.relevance !== tier) {
      tier = hit.relevance;
      const label = make("li", "hits-divider");
      label.append(make("span", "", RELEVANCE_LABELS[tier] || RELEVANCE_LABELS[3]));
      rows.push(label);
    }
    const b = make("button", "hit");
    b.type = "button";
    b.title = hit.url;
    b.setAttribute("aria-pressed", String(state.lyricsHit === hit.url));
    const main = make("span", "hit-main");
    main.append(make("span", "hit-title", hit.title), make("span", "hit-sub", hit.artist));
    b.append(make("span", "site", hit.site), main);
    if (hit.translated) b.append(make("span", "badge", "中英"));
    b.addEventListener("click", () => chooseLyrics(hit));
    rows.push(hitRow(b, [pickButton(hit)]));
  }
  if (!hits.length) {
    rows.push(make("li", "hits-empty", "找不到歌詞，可換個關鍵字（例如加上歌手），或改用「貼上網址」"));
  }
  // LyricsTranslate 的自動搜尋常被擋，永遠提供在擷取視窗中自行搜尋的入口
  const browse = make("button", "hit hit-browse");
  browse.type = "button";
  const main = make("span", "hit-main");
  main.append(
    make("span", "hit-title", "在瀏覽器搜尋 LyricsTranslate 的英文翻譯"),
    make("span", "hit-sub", "在開啟的視窗中點選翻譯頁，載入後自動擷取中英對照"),
  );
  browse.append(make("span", "site", "LyricsTranslate"), main, make("span", "badge", "中英"));
  browse.addEventListener("click", browseLyricsTranslate);
  rows.push(hitRow(browse, [pickButton(browseHit())]));
  el.lyricsHits.replaceChildren(...rows);
}

/** 「＋」：選取以合併中英 */
function pickButton(hit) {
  const order = state.mergePicks.findIndex((p) => p.url === hit.url);
  const pick = make("button", "hit-open hit-pick", order >= 0 ? String(order + 1) : "＋");
  pick.type = "button";
  pick.title = "任選兩個歌詞，自動判斷原文與英譯並合併成中英對照";
  pick.setAttribute("aria-pressed", String(order >= 0));
  pick.setAttribute("aria-label", `選取以合併：${hit.title}`);
  pick.addEventListener("click", () => toggleMergePick(hit));
  return pick;
}

/** DuckDuckGo 的 LyricsTranslate 站內搜尋頁 */
function browseUrl() {
  const q = `site:lyricstranslate.com ${state.lastQuery} English translation`;
  return `https://duckduckgo.com/?q=${encodeURIComponent(q)}`;
}

/** 「在瀏覽器搜尋 LyricsTranslate」也能選來合併：抓取時開啟視窗讓使用者點選翻譯頁 */
function browseHit() {
  return { site: "LyricsTranslate", title: "在瀏覽器搜尋 LyricsTranslate 的英文翻譯", url: browseUrl(), browse: true };
}

/** 以擷取視窗開啟 DuckDuckGo 的站內搜尋，使用者點進翻譯頁後自動擷取 */
async function browseLyricsTranslate() {
  if (state.busy || !state.lastQuery) return;
  const url = browseUrl();
  state.lyricsHit = null;
  renderLyricsHits();
  clearMessages();
  setBusy(true, "請在擷取視窗中點選 LyricsTranslate 的英文翻譯頁，頁面載入後會自動擷取");
  try {
    const song = await invoke("capture_song", { url, columnsOnly: true });
    // 之後按「＋」合併時直接沿用，不必再點選一次
    state.songCache.set(url, song);
    showPreview(song, null);
    if (!WIDE.matches) el.preview.scrollIntoView({ behavior: "smooth", block: "start" });
  } catch (e) {
    showError(e, [{ label: "重試", run: browseLyricsTranslate }]);
  } finally {
    setBusy(false);
  }
}

function toggleMergePick(hit) {
  if (state.busy) return;
  const picks = state.mergePicks.filter((p) => p.url !== hit.url);
  if (picks.length === state.mergePicks.length) {
    picks.push(hit);
    // 已選兩個時換掉最早選的
    if (picks.length > 2) picks.shift();
  }
  state.mergePicks = picks;
  renderLyricsHits();
  if (picks.length === 2) mergePicked();
}

/** 一般請求或內嵌瀏覽器抓取；同一次搜尋中抓過的直接沿用 */
async function loadSong(hit) {
  const url = hit.url;
  if (state.songCache.has(url)) return state.songCache.get(url);
  const song = hit.browse
    ? await invoke("capture_song", { url, columnsOnly: true })
    : await invoke(needsBrowser(url) ? "capture_song" : "fetch_song", { url });
  state.songCache.set(url, song);
  return song;
}

/** 抓取選取的兩個歌詞，自動判斷原文與英譯並合併；語言重複時去重 */
async function mergePicked() {
  const picks = [...state.mergePicks];
  clearMessages();
  const songs = [];
  try {
    // 擷取視窗同時只能有一個，依序抓取
    for (const [i, hit] of picks.entries()) {
      setBusy(true, hit.browse
        ? `第 ${i + 1} 個：請在擷取視窗中點選 LyricsTranslate 的英文翻譯頁，載入後自動擷取`
        : `正在抓取第 ${i + 1} 個歌詞（${hit.site}）…`);
      songs.push(await loadSong(hit));
    }
    const src = (s) => ({ lyrics: s.lyrics, bilingual: s.bilingual ?? null });
    const c = await invoke("combine_lyrics", { first: src(songs[0]), second: src(songs[1]) });
    state.lyricsHit = null;
    renderLyricsHits();
    showPreview({ ...songs[c.primary], lyrics: c.lyrics, bilingual: c.bilingual, note: c.note }, null);
    if (!WIDE.matches) el.preview.scrollIntoView({ behavior: "smooth", block: "start" });
  } catch (e) {
    const failed = picks[songs.length];
    showError(failed ? `第 ${songs.length + 1} 個歌詞（${failed.site}）抓取失敗
${e}` : e, [
      { label: "重試", run: () => !state.busy && mergePicked() },
    ]);
  } finally {
    setBusy(false);
  }
}

function renderVideoHits() {
  const res = state.lastVideos;
  if (!res) return;
  el.videoResearch.hidden = el.preview.hidden || !el.title.value.trim();
  if (res.status === "rejected") {
    el.videoNote.textContent = "";
    el.videoHits.replaceChildren(make("li", "hits-empty", String(res.reason)));
    return;
  }
  const videos = res.value;
  el.videoNote.textContent = state.video ? "已選擇 · 再點一次取消" : videos.length ? "點選要填入的伴奏" : "";
  if (!videos.length) {
    el.videoHits.replaceChildren(make("li", "hits-empty", "找不到伴奏影片"));
    return;
  }
  el.videoHits.replaceChildren(
    ...videos.map((v) => {
      const b = make("button", "hit");
      b.type = "button";
      b.setAttribute("aria-pressed", String(state.video?.url === v.url));
      const main = make("span", "hit-main");
      main.append(make("span", "hit-title", v.title), make("span", "hit-sub", v.channel));
      b.append(main, make("span", "hit-time", v.duration));
      b.addEventListener("click", () => chooseVideo(v));
      const open = make("button", "hit-open", "↗");
      open.type = "button";
      open.title = "在 YouTube 開啟";
      open.setAttribute("aria-label", `在 YouTube 開啟：${v.title}`);
      open.addEventListener("click", () => openPage(v.url));
      return hitRow(b, [open]);
    }),
  );
}

async function chooseLyrics(hit) {
  if (state.busy) return;
  state.lyricsHit = hit.url;
  renderLyricsHits();
  // 單欄時預覽在下方，捲過去；雙欄時本來就看得到
  if ((await fetchPreview("auto", hit.url)) && !WIDE.matches) {
    el.preview.scrollIntoView({ behavior: "smooth", block: "start" });
  }
}

function chooseVideo(v) {
  state.video = state.video?.url === v.url ? null : v;
  renderVideoHits();
  applyVideo();
  renderFields();
}

// ─── 標籤 ────────────────────────────────────────────

/** @returns {"ko" | "ja" | "zh" | "en" | null} */
function detectLanguage(text) {
  const count = (re) => (text.match(re) || []).length;
  const hangul = count(/[가-힯]/g);
  const kana = count(/[぀-ヿ]/g);
  const han = count(/[一-鿿]/g);
  const latin = count(/[a-z]/gi);
  // 中英格式的英譯不算進語言判斷
  const total = hangul + kana + han + (el.bilingual.checked ? 0 : latin);
  if (total < 20) return null;
  if (hangul / total > 0.2) return "ko";
  // 日文歌詞也有大量漢字，假名比例不必高
  if (kana / total > 0.08) return "ja";
  if (han > (el.bilingual.checked ? 0 : latin)) return "zh";
  return latin ? "en" : null;
}

function isLanguageField(field) {
  return field.options.some((o) => Object.values(LANGUAGE_OPTIONS).some((re) => re.test(o.name)));
}

/** 未被手動選過的語言欄位：依歌詞自動選取對應選項 */
function autoSelectTags() {
  const re = LANGUAGE_OPTIONS[detectLanguage(el.lyrics.value)];
  for (const field of state.tagFields || []) {
    if (state.touched.has(field.name) || !isLanguageField(field)) continue;
    const match = re && field.options.find((o) => re.test(o.name));
    if (match) state.selected.set(field.name, new Set([match.name]));
    else state.selected.delete(field.name);
  }
}

function toggleTag(field, name) {
  state.touched.add(field.name);
  const set = state.selected.get(field.name) || new Set();
  if (set.has(name)) set.delete(name);
  else {
    if (!field.multi) set.clear();
    set.add(name);
  }
  state.selected.set(field.name, set);
  renderTags();
}

function renderTags() {
  const fields = (state.tagFields || []).filter((f) => f.options.length);
  el.tags.hidden = fields.length === 0;
  el.tags.replaceChildren(
    ...fields.map((field) => {
      const row = make("div", "tag-row");
      row.setAttribute("role", "group");
      row.setAttribute("aria-label", field.name);
      const label = make("span", "tag-label", field.name);
      label.title = field.multi ? `${field.name}（可複選）` : `${field.name}（單選，再點一次取消）`;
      const chips = make("div", "chips");
      const chosen = state.selected.get(field.name) || new Set();
      for (const opt of field.options) {
        const color = NOTION_COLORS.includes(opt.color) ? opt.color : "default";
        const b = make("button", `chip c-${color}`, opt.name);
        b.type = "button";
        b.setAttribute("aria-pressed", String(chosen.has(opt.name)));
        b.addEventListener("click", () => toggleTag(field, opt.name));
        chips.append(b);
      }
      row.append(label, chips);
      return row;
    }),
  );
}

function setTagFields(fields) {
  state.tagFields = fields;
  // 資料庫結構變更後，移除已不存在的欄位或選項
  for (const [name, set] of state.selected) {
    const field = fields.find((f) => f.name === name);
    if (!field) state.selected.delete(name);
    else for (const v of set) if (!field.options.some((o) => o.name === v)) set.delete(v);
  }
  autoSelectTags();
  renderTags();
}

function chosenTags() {
  return [...state.selected]
    .filter(([, set]) => set.size)
    .map(([property, set]) => ({ property, values: [...set] }));
}

// ─── 文字欄位（伴奏、備註） ──────────────────────────

function accompanimentField() {
  return (state.textFields || []).find((f) => ACCOMPANIMENT_FIELD.test(f));
}

/** 把選中的影片寫入伴奏欄位；使用者自己打過的文字不覆蓋 */
function applyVideo() {
  const field = accompanimentField();
  if (!field) return;
  const cur = state.texts.get(field) || { text: "", link: "", note: "" };
  const keepText = state.touchedTexts.has(field);
  const v = state.video;
  state.texts.set(field, {
    text: keepText ? cur.text : v ? guessAccompaniment(v.title) : "",
    link: v ? v.url : "",
    note: v ? v.title : "",
  });
}

function setText(field, patch) {
  state.texts.set(field, { text: "", link: "", note: "", ...state.texts.get(field), ...patch });
}

function suggestionsFor(field) {
  const known = state.suggestions.get(field) || [];
  const extra = ACCOMPANIMENT_FIELD.test(field) ? ACCOMPANIMENT_LABELS : [];
  return [...new Set([...known, ...extra])];
}

function fillDatalist(list, field) {
  list.replaceChildren(...suggestionsFor(field).map((v) => Object.assign(document.createElement("option"), { value: v })));
}

function renderFields() {
  const fields = state.textFields || [];
  el.fields.hidden = fields.length === 0 || el.preview.hidden;
  el.fields.replaceChildren(
    ...fields.map((field, i) => {
      const accompaniment = ACCOMPANIMENT_FIELD.test(field);
      const v = state.texts.get(field) || { text: "", link: "", note: "" };
      const row = make("div", "tag-row field-row");
      const label = make("span", "tag-label", field);
      label.title = field;
      const box = make("div", "field-inputs");

      const listId = `field-list-${i}`;
      const list = make("datalist");
      list.id = listId;
      list.dataset.field = field;
      fillDatalist(list, field);

      const text = make("input", "text-input");
      text.value = v.text;
      text.placeholder = accompaniment ? "純伴奏 / 和聲伴奏" : `${field}（選填）`;
      text.spellcheck = false;
      text.setAttribute("list", listId);
      text.setAttribute("aria-label", field);
      text.addEventListener("input", () => {
        state.touchedTexts.add(field);
        setText(field, { text: text.value });
      });
      box.append(text, list);

      if (accompaniment) {
        const link = make("input", "text-input link-input");
        link.value = v.link;
        link.type = "url";
        link.placeholder = "YouTube 連結（從上方伴奏清單點選）";
        link.spellcheck = false;
        link.setAttribute("aria-label", `${field}連結`);
        link.addEventListener("input", () => setText(field, { link: link.value }));
        box.append(link);
        if (v.note) box.append(make("span", "video-note", `▶ ${v.note}`));
      }
      row.append(label, box);
      return row;
    }),
  );
}

async function loadSuggestions() {
  if (state.suggestionsLoaded || !isConfigured()) return;
  state.suggestionsLoaded = true;
  try {
    for (const f of await invoke("field_suggestions")) state.suggestions.set(f.property, f.values);
    // 只更新建議清單，不重繪輸入框（避免打字中失去焦點）
    for (const list of el.fields.querySelectorAll("datalist")) fillDatalist(list, list.dataset.field);
  } catch {
    state.suggestionsLoaded = false;
  }
}

function setSchema(info) {
  setTagFields(info.tags);
  state.textFields = info.texts || [];
  applyVideo();
  renderFields();
  loadSuggestions();
}

function resetSchema() {
  state.tagFields = null;
  state.textFields = null;
  state.suggestions = new Map();
  state.suggestionsLoaded = false;
}

/** 讀取資料庫的標籤與文字欄位；失敗時不顯示（匯入時會顯示錯誤） */
async function loadSchema() {
  if (!isConfigured()) return setSchema({ tags: [], texts: [] });
  if (state.tagFields && state.textFields) {
    renderTags();
    renderFields();
    return;
  }
  try {
    setSchema(await invoke("get_schema"));
  } catch {
    el.tags.hidden = true;
    el.fields.hidden = true;
  }
}

function chosenTexts() {
  return (state.textFields || [])
    .map((property) => {
      const v = state.texts.get(property) || {};
      return { property, text: (v.text || "").trim(), link: (v.link || "").trim() || null };
    })
    .filter((t) => t.text || t.link);
}

// ─── 抓取 / 匯入 ─────────────────────────────────────

function showPreview(song, input) {
  el.title.value = song.title;
  el.artist.value = song.artist;
  // 來源附英譯時預設使用中英格式
  el.lyrics.value = song.bilingual || song.lyrics;
  el.bilingual.checked = Boolean(song.bilingual);
  state.plainLyrics = song.bilingual ? song.lyrics : null;
  state.bilingualText = song.bilingual || null;
  state.previewUrl = song.sourceUrl;
  state.previewInput = input;
  el.previewNote.textContent = song.note || "";
  el.previewNote.hidden = !song.note;
  el.preview.hidden = false;
  el.previewEmpty.hidden = true;
  el.lyrics.scrollTop = 0;
  // 每首歌重新挑標籤與欄位；搜尋時選好的伴奏保留
  state.selected = new Map();
  state.touched = new Set();
  state.texts = new Map();
  state.touchedTexts = new Set();
  applyVideo();
  autoSelectTags();
  loadSchema();
  renderVideoHits();
  updateStats();
  requestAnimationFrame(autosize);
  autoConvert();
}

/** 自動轉繁：日文、韓文歌詞不轉（日文漢字會被改成繁體字形） */
function autoConvert() {
  const lang = detectLanguage(el.lyrics.value);
  if (el.autoHant.checked && lang !== "ja" && lang !== "ko") convertChinese("hant");
}

/**
 * @param {"auto" | "browser"} mode auto：一般請求（KKBOX 等自動改用瀏覽器）；browser：強制瀏覽器擷取
 * @param {string | null} target 搜尋結果的網址；null 則使用網址列
 */
async function fetchPreview(mode = "auto", target = null) {
  const url = (target ?? el.url.value).trim();
  if (!url) return el.url.focus(), false;
  const viaBrowser = mode === "browser" || needsBrowser(url);
  clearMessages();
  setBusy(true, viaBrowser ? "正在背景載入頁面…若需要驗證，會跳出視窗讓你完成" : "正在抓取網頁…");
  try {
    const song = await invoke(viaBrowser ? "capture_song" : "fetch_song", { url });
    if (target !== null) state.songCache.set(url, song);
    showPreview(song, target === null ? url : null);
    if (!song.artist) el.artist.focus();
    return true;
  } catch (e) {
    const fallback = [
      viaBrowser
        ? { label: "重試", run: () => fetchPreview("browser", target) }
        : { label: "用瀏覽器擷取", run: () => fetchPreview("browser", target) },
      { label: "手動輸入", run: manualEntry },
    ];
    showError(e, fallback);
    return false;
  } finally {
    setBusy(false);
  }
}

/** 手動輸入：開空白預覽，網址列有合法網址就當作來源網址 */
function manualEntry() {
  const input = el.url.value.trim();
  const url = state.mode === "url" && /^https?:\/\//i.test(input) && hostOf(input) ? input : "";
  clearMessages();
  showPreview({ title: "", artist: "", lyrics: "", sourceUrl: url }, input);
  el.lyrics.placeholder = "在此貼上歌詞，匯入時會自動整理空行與製作資訊";
  el.title.focus();
}

async function toggleBilingual() {
  if (el.bilingual.checked) {
    try {
      const converted = await invoke("to_bilingual", { lyrics: el.lyrics.value });
      if (converted && converted !== el.lyrics.value) {
        state.plainLyrics = el.lyrics.value;
        state.bilingualText = converted;
        el.lyrics.value = converted;
      }
    } catch (e) {
      showError(e);
    }
  } else if (state.plainLyrics !== null && el.lyrics.value === state.bilingualText) {
    // 沒改過才還原，避免蓋掉使用者的修改
    el.lyrics.value = state.plainLyrics;
    state.plainLyrics = null;
    state.bilingualText = null;
  }
  updateStats();
  autosize();
  autoSelectTags();
  renderTags();
}

/** 歌名、歌手、歌詞一起轉換；中英格式的還原用原文也一併轉換 */
async function convertChinese(to) {
  const convert = (text) => (text ? invoke("convert_chinese", { text, to }) : Promise.resolve(text));
  try {
    const [title, artist, lyrics, plain] = await Promise.all([
      convert(el.title.value),
      convert(el.artist.value),
      convert(el.lyrics.value),
      convert(state.plainLyrics),
    ]);
    const unchangedBilingual = state.bilingualText !== null && el.lyrics.value === state.bilingualText;
    el.title.value = title;
    el.artist.value = artist;
    el.lyrics.value = lyrics;
    if (state.plainLyrics !== null) {
      state.plainLyrics = plain;
      state.bilingualText = unchangedBilingual ? lyrics : state.bilingualText;
    }
    updateStats();
  } catch (e) {
    showError(e);
  }
}

async function importSong() {
  if (!isConfigured()) {
    showError("尚未完成設定\n請先填入 Notion API Key 與 Database ID");
    openSettings();
    return;
  }
  clearMessages();
  setBusy(true, "正在寫入 Notion…");
  try {
    const texts = chosenTexts();
    const res = await invoke("import_song", {
      song: {
        title: el.title.value,
        artist: el.artist.value,
        lyrics: el.lyrics.value,
        sourceUrl: state.previewUrl,
      },
      tags: chosenTags(),
      texts,
      bilingual: el.bilingual.checked,
      writeBody: el.writeBody.checked,
    });
    state.lastPageUrl = res.url;
    const summary = [...chosenTags().flatMap((t) => t.values), ...texts.map((t) => t.text).filter(Boolean)].join(" · ");
    el.resultText.textContent = summary;
    el.resultText.hidden = !summary;
    // 檢視若依標籤篩選或分組，沒填的頁面可能看不到，提醒一下
    const empty = (state.tagFields || []).filter((f) => f.options.length && !state.selected.get(f.name)?.size);
    const hints = empty.map((f) => `「${f.name}」未選擇；若 Notion 檢視依此欄位篩選，頁面可能不會出現在該檢視`);
    el.resultWarnings.replaceChildren(...[...res.warnings, ...hints].map((w) => make("li", "", w)));
    el.result.hidden = false;
    // 剛用過的值加入建議
    for (const t of texts) {
      if (t.text) state.suggestions.set(t.property, [...new Set([t.text, ...(state.suggestions.get(t.property) || [])])]);
    }
    addHistory({ title: el.title.value.trim(), artist: el.artist.value.trim(), url: res.url });
  } catch (e) {
    showError(e);
  } finally {
    setBusy(false);
  }
}

/** 一鍵匯入：預覽仍對應目前網址就直接匯入（保留使用者的修改），否則先抓取 */
async function quickImport() {
  if (state.busy || state.mode !== "url") return;
  const current = el.url.value.trim();
  if (el.preview.hidden || state.previewInput !== current) {
    if (!(await fetchPreview())) return;
  }
  // 等欄位讀完，自動判斷的語言才會一起送出
  await loadSchema();
  await importSong();
}

// ─── 歷史 ────────────────────────────────────────────

function loadHistory() {
  try {
    const list = JSON.parse(localStorage.getItem(HISTORY_KEY) || "[]");
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

function addHistory(item) {
  const list = [{ ...item, at: Date.now() }, ...loadHistory().filter((h) => h.url !== item.url)];
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(list.slice(0, HISTORY_MAX)));
  } catch { /* 儲存空間不可用時只是不保留歷史 */ }
  renderHistory();
}

function relativeTime(ts) {
  const s = Math.round((Date.now() - ts) / 1000);
  if (s < 60) return "剛剛";
  if (s < 3600) return `${Math.floor(s / 60)} 分鐘前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小時前`;
  return new Date(ts).toLocaleDateString();
}

function renderHistory() {
  const list = loadHistory();
  el.historyWrap.hidden = list.length === 0;
  el.history.replaceChildren(
    ...list.map((h) => {
      const li = document.createElement("li");
      const btn = document.createElement("button");
      btn.type = "button";
      btn.title = "在 Notion 開啟";
      btn.append(make("span", "h-title", h.title), make("span", "h-artist", h.artist || ""), make("span", "h-time", relativeTime(h.at)));
      btn.addEventListener("click", () => openPage(h.url));
      li.append(btn);
      return li;
    }),
  );
}

async function openPage(url) {
  try {
    await invoke("open_link", { url });
  } catch (e) {
    showError(e);
  }
}

// ─── 設定 ────────────────────────────────────────────

function fillSettings(s) {
  state.settings = s;
  for (const [k, input] of Object.entries(el.s)) {
    if (k !== "apiKey") input.value = s[k] ?? "";
  }
  el.s.apiKey.value = "";
  el.s.apiKey.placeholder = s.hasApiKey ? "••••••••（已儲存，留空則不變更）" : "ntn_…";
  el.keyState.textContent = s.hasApiKey ? "已安全存放於系統認證管理員" : "金鑰只會存放在系統認證管理員，不寫入任何檔案";
  el.clearKey.hidden = !s.hasApiKey;
  el.writeBody.checked = s.writeBody;
  el.banner.hidden = isConfigured();
}

function settingsPayload(extra = {}) {
  const out = { writeBody: el.writeBody.checked, apiKey: el.s.apiKey.value || null, clearApiKey: false };
  for (const [k, input] of Object.entries(el.s)) if (k !== "apiKey") out[k] = input.value;
  return { ...out, ...extra };
}

function settingsMessage(kind, text, warnings = []) {
  el.settingsMsg.className = `settings-msg ${kind}`;
  el.settingsMsg.textContent = text;
  for (const w of warnings) el.settingsMsg.append(make("div", "warn", `! ${w}`));
  el.settingsMsg.hidden = false;
}

async function saveSettings(extra) {
  const s = await invoke("save_settings", { input: settingsPayload(extra) });
  fillSettings(s);
  return s;
}

function openSettings() {
  el.settingsMsg.hidden = true;
  el.sheet.classList.add("open");
  el.sheet.setAttribute("aria-hidden", "false");
  setTimeout(() => (state.settings?.hasApiKey ? el.s.databaseId : el.s.apiKey).focus(), 150);
}

function closeSettings() {
  el.sheet.classList.remove("open");
  el.sheet.setAttribute("aria-hidden", "true");
  el.url.focus();
}

// ─── 事件 ────────────────────────────────────────────

el.modeSearch.addEventListener("click", () => { setMode("search"); el.url.focus(); });
el.modeUrl.addEventListener("click", () => { setMode("url"); el.url.focus(); });

el.urlForm.addEventListener("submit", (e) => {
  e.preventDefault();
  if (state.busy) return;
  if (state.mode === "search") runSearch();
  else fetchPreview();
});

el.url.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && state.mode === "url") {
    e.preventDefault();
    quickImport();
  }
});

el.quickImport.addEventListener("click", quickImport);
document.querySelectorAll(".manual-btn").forEach((b) => b.addEventListener("click", () => !state.busy && manualEntry()));
el.importBtn.addEventListener("click", () => !state.busy && importSong());
el.videoResearch.addEventListener("click", researchVideos);
el.bilingual.addEventListener("change", toggleBilingual);
el.autoHant.addEventListener("change", () => {
  try { localStorage.setItem(AUTO_HANT_KEY, el.autoHant.checked ? "1" : "0"); } catch { /* 只是不記住 */ }
  if (!el.preview.hidden) autoConvert();
});
el.resultOpen.addEventListener("click", () => state.lastPageUrl && openPage(state.lastPageUrl));
el.lyrics.addEventListener("input", () => {
  updateStats();
  autosize();
  // 手動貼上歌詞後也能自動判斷語言
  autoSelectTags();
  renderTags();
});
WIDE.addEventListener("change", autosize);
el.title.addEventListener("input", () => {
  el.videoResearch.hidden = !state.lastVideos || !el.title.value.trim();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && el.sheet.classList.contains("open")) closeSettings();
  if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && !el.preview.hidden && e.target !== el.url) {
    e.preventDefault();
    if (!state.busy) importSong();
  }
});

el.writeBody.addEventListener("change", async () => {
  if (!state.settings) return;
  try {
    // 只更新此開關，其餘設定沿用目前已儲存的值
    const s = state.settings;
    await invoke("save_settings", { input: { ...s, writeBody: el.writeBody.checked, apiKey: null, clearApiKey: false } });
    state.settings.writeBody = el.writeBody.checked;
  } catch (e) {
    showError(e);
  }
});

el.historyClear.addEventListener("click", () => {
  try { localStorage.removeItem(HISTORY_KEY); } catch { /* 忽略 */ }
  renderHistory();
});

$("open-settings").addEventListener("click", openSettings);
$("setup-open").addEventListener("click", openSettings);
el.sheet.querySelectorAll("[data-close]").forEach((n) => n.addEventListener("click", closeSettings));

el.settingsForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  try {
    await saveSettings();
    settingsMessage("ok", "已儲存");
    // 可能換了資料庫，欄位重新讀取
    resetSchema();
    if (!el.preview.hidden) loadSchema();
  } catch (err) {
    settingsMessage("err", String(err));
  }
});

el.testConn.addEventListener("click", async () => {
  el.testConn.disabled = true;
  settingsMessage("ok", "連線中…");
  try {
    await saveSettings(); // 先存，確保測試的是畫面上的值
    const info = await invoke("test_connection");
    settingsMessage("ok", `✓ 連線成功\n${info.summary}`, info.warnings);
    resetSchema();
    setSchema(info);
  } catch (err) {
    settingsMessage("err", String(err));
  } finally {
    el.testConn.disabled = false;
  }
});

el.clearKey.addEventListener("click", async () => {
  try {
    await saveSettings({ apiKey: null, clearApiKey: true });
    settingsMessage("ok", "已移除 API Key");
  } catch (err) {
    settingsMessage("err", String(err));
  }
});

// 拖放連結（tauri.conf.json 已關閉原生檔案拖放，讓 WebView 收到事件）
let dragDepth = 0;
window.addEventListener("dragenter", (e) => { e.preventDefault(); dragDepth++; document.body.classList.add("dragging"); });
window.addEventListener("dragleave", () => { if (--dragDepth <= 0) { dragDepth = 0; document.body.classList.remove("dragging"); } });
window.addEventListener("dragover", (e) => e.preventDefault());
window.addEventListener("drop", (e) => {
  e.preventDefault();
  dragDepth = 0;
  document.body.classList.remove("dragging");
  const text = (e.dataTransfer.getData("text/uri-list") || e.dataTransfer.getData("text/plain") || "")
    .split("\n").map((l) => l.trim()).find((l) => /^https?:\/\//i.test(l));
  if (text) {
    setMode("url");
    el.url.value = text;
    if (!state.busy) fetchPreview();
  }
});

// ─── 啟動 ────────────────────────────────────────────

(async () => {
  let mode = "search";
  try { mode = localStorage.getItem(MODE_KEY) === "url" ? "url" : "search"; } catch { /* 用預設 */ }
  setMode(mode);
  try { el.autoHant.checked = localStorage.getItem(AUTO_HANT_KEY) !== "0"; } catch { /* 用預設（開啟） */ }
  renderHistory();
  try {
    fillSettings(await invoke("get_settings"));
  } catch (e) {
    showError(e);
  }
})();
