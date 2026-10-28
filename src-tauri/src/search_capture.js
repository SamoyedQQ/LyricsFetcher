// 注入 LyricsTranslate 搜尋視窗的腳本：讀取 DuckDuckGo 搜尋頁的結果連結並送回 Rust。
//
// 與 capture.js 相同：視窗沒有任何 Tauri IPC 權限，資料以導向保留網域的方式傳回，
// 由 Rust 的 on_navigation 攔截並取消。
(() => {
  if (window.top !== window) return;

  const ENDPOINT = "https://capture.lyricsfetcher.invalid/";
  const POLL_MS = 300;
  // 超過這個時間仍沒有結果，就顯示視窗讓使用者看到發生什麼事（例如人機驗證）
  const SHOW_AFTER_MS = 12000;
  const MAX_LINKS = 30;

  let sent = false;
  let shown = false;
  const started = Date.now();

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
    const links = [...document.querySelectorAll("a.result__a")].slice(0, MAX_LINKS).map((a) => ({
      href: a.getAttribute("href") || "",
      text: (a.textContent || "").trim(),
    }));
    if (links.length) return send({ links });
    // 沒有任何結果
    if (document.querySelector(".no-results, .result--no-result")) return send({ links: [] });
    if (document.querySelector(".anomaly-modal__modal, #anomaly-modal") || Date.now() - started > SHOW_AFTER_MS) {
      requestShow();
    }
    setTimeout(tick, POLL_MS);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", tick, { once: true });
  } else {
    tick();
  }
})();
