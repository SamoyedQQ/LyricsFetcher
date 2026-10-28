// 注入擷取視窗的腳本：等歌詞區塊載入完成後，把資料送回 Rust。
//
// 這個視窗載入的是外部網站，沒有任何 Tauri IPC 權限（capabilities 只授權 main 視窗）。
// 回傳方式是導向一個保留網域（.invalid 永遠不會解析），
// 由 Rust 的 on_navigation 攔截並取消這次導向，資料不會離開本機。
(() => {
  if (window.top !== window) return;

  const ENDPOINT = "https://capture.lyricsfetcher.invalid/";
  const CUSTOM_SELECTOR = __LYRICS_SELECTOR__; // 由 Rust 以 JSON 字串注入，可能為 null
  // 瀏覽模式：使用者在視窗中自行搜尋、點選，只在出現左右對照的歌詞頁時擷取，不會逾時放棄
  const COLUMNS_ONLY = __COLUMNS_ONLY__;
  const LYRICS_SELECTORS = [".track-content .lyrics", "div.lyrics", "[class*='lyric' i]", "[id*='lyric' i]"];
  const POLL_MS = 400;
  const GIVE_UP_MS = 25000;
  // 背景載入超過這個時間仍沒抓到歌詞，就顯示視窗讓使用者看到發生什麼事
  const SHOW_AFTER_MS = 10000;
  const MAX_HTML = 200000;

  let sent = false;
  let shown = false;
  let last = "";
  let stableTicks = 0;
  const started = Date.now();
  /** 第一次找到左右對照列的時間；原文欄可能較晚載入 */
  let rowsSince = 0;

  // AWS WAF（KKBOX）與 Cloudflare（LyricsTranslate）的驗證頁：等使用者完成
  const isChallenge = () =>
    document.title === "Human Verification" ||
    /^Just a moment/i.test(document.title) ||
    !!document.querySelector("#captcha-container, .amzn-captcha-state-container, #challenge-form, .cf-turnstile") ||
    typeof window.AwsWafIntegration !== "undefined";

  const textOf = (el) => (el ? (el.innerText || el.textContent || "").trim() : "");
  const meta = (p) => document.querySelector(`meta[property="${p}"], meta[name="${p}"]`)?.content?.trim() || "";
  const lineCount = (t) => t.split("\n").filter((l) => l.trim()).length;
  const hasCjk = (t) => /[぀-ヿ㐀-鿿가-힯]/.test(t);
  // 只找到英譯時，多等這段時間讓原文欄載入
  const WAIT_ORIGINAL_MS = 6000;

  function findLyrics() {
    if (CUSTOM_SELECTOR) {
      try {
        return document.querySelector(CUSTOM_SELECTOR);
      } catch {
        return null;
      }
    }
    for (const sel of LYRICS_SELECTORS) {
      // 同一 selector 多個命中時取行數最多者（行數相同時取較內層的元素）
      let best = null;
      let bestLines = 0;
      for (const el of document.querySelectorAll(sel)) {
        const n = lineCount(textOf(el));
        if (n >= bestLines && n > 0) {
          best = el;
          bestLines = n;
        }
      }
      if (best && bestLines >= 3) return best;
    }
    return null;
  }

  /**
   * 左右對照頁（LyricsTranslate）：原文與譯文的每一行都有相同的 class「ll-段-行」，依 class 分組。
   * 原文欄可能排在譯文之後、重複出現或只有拼音，所以送出同一行的所有文字，由 Rust 判斷哪個是原文。
   * @returns {{p: number, texts: string[]}[]}
   */
  function findColumns() {
    const groups = new Map();
    for (const el of document.querySelectorAll("[class*='ll-']")) {
      const cls = [...el.classList].find((c) => /^ll-\d+-\d+$/.test(c));
      if (!cls) continue;
      if (!groups.has(cls)) groups.set(cls, []);
      const text = textOf(el);
      const texts = groups.get(cls);
      if (text && !texts.includes(text)) texts.push(text);
    }
    return [...groups].map(([cls, texts]) => ({ p: Number(cls.split("-")[1]), texts }));
  }

  function findTitleArtist() {
    let title = textOf(document.querySelector("main h1, h1"));
    let artist = textOf(document.querySelector("main a[href*='/artist/']"));
    if (!title || !artist) {
      // og:title 常見格式：「歌名 - 歌手」
      const og = (meta("og:title") || document.title).replace(/\s*[-|｜–]\s*KKBOX.*$/i, "");
      const parts = og.split(/\s+[-–]\s+/);
      title ||= parts[0] || "";
      artist ||= parts.length > 1 ? parts.slice(1).join(" - ") : "";
    }
    return { title, artist };
  }

  /** 請 Rust 顯示（背景中的）擷取視窗；這次導向會被取消，頁面不受影響 */
  function requestShow() {
    if (shown || sent) return;
    shown = true;
    location.href = ENDPOINT + "show";
  }

  function send(payload) {
    if (sent) return;
    sent = true;
    location.href = ENDPOINT + "?d=" + encodeURIComponent(JSON.stringify(payload));
  }

  function tick() {
    if (sent) return;
    if (isChallenge()) {
      // 驗證頁面：顯示視窗等使用者完成，完成後頁面會重新載入、腳本重新執行
      requestShow();
      setTimeout(tick, POLL_MS);
      return;
    }
    const rows = findColumns();
    if (rows.length >= 3) {
      const snapshot = JSON.stringify(rows);
      stableTicks = snapshot === last ? stableTicks + 1 : 0;
      last = snapshot;
      rowsSince ||= Date.now();
      const original = rows.some((r) => r.texts.some(hasCjk));
      if (stableTicks >= 2 && (original || Date.now() - rowsSince > WAIT_ORIGINAL_MS)) {
        // 網址長度有限：超過上限就只送前面的行
        let kept = rows;
        while (kept.length > 3 && JSON.stringify(kept).length > MAX_HTML) kept = kept.slice(0, Math.floor(kept.length * 0.8));
        send({ ...findTitleArtist(), pageTitle: document.title, rows: kept });
        return;
      }
      setTimeout(tick, POLL_MS);
      return;
    }
    if (COLUMNS_ONLY) {
      setTimeout(tick, POLL_MS);
      return;
    }
    if (Date.now() - started > SHOW_AFTER_MS) requestShow();
    const el = findLyrics();
    const text = textOf(el);
    if (el && text) {
      // 內容連續兩次不變才送出，避免抓到載入到一半的歌詞
      stableTicks = text === last ? stableTicks + 1 : 0;
      last = text;
      if (stableTicks >= 2) {
        const { title, artist } = findTitleArtist();
        send({ title, artist, pageTitle: document.title, lyricsHtml: el.outerHTML.slice(0, MAX_HTML), lyricsText: text.slice(0, MAX_HTML) });
        return;
      }
    } else if (Date.now() - started > GIVE_UP_MS && document.readyState === "complete") {
      send({ error: "頁面中找不到歌詞區塊", ...findTitleArtist() });
      return;
    }
    setTimeout(tick, POLL_MS);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", tick, { once: true });
  } else {
    tick();
  }
})();
