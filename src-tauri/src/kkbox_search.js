// 注入 KKBOX 搜尋視窗的腳本：在 KKBOX 網頁內呼叫站內搜尋 API，把結果送回 Rust。
//
// KKBOX 的 AWS WAF 會擋程式直接發出的請求（回應 202 + x-amzn-waf-action: challenge），
// 在瀏覽器中載入頁面時驗證會自動完成並取得 token cookie，之後同網域的 fetch 就能正常回應。
// 與 capture.js 相同：視窗沒有任何 Tauri IPC 權限，資料以導向保留網域的方式傳回。
(() => {
  if (window.top !== window) return;

  const ENDPOINT = "https://capture.lyricsfetcher.invalid/";
  const QUERY = __QUERY__; // 由 Rust 以 JSON 字串注入
  const RETRY_MS = 800;
  // 超過這個時間仍未通過驗證，就顯示視窗讓使用者完成（例如需要圖形驗證）
  const SHOW_AFTER_MS = 12000;
  const MAX_RESULTS = 30;

  let sent = false;
  let shown = false;
  let busy = false;
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

  /** @returns {Promise<boolean>} 是否已送出結果 */
  async function search() {
    const params = new URLSearchParams({ q: QUERY, terr: "tw", lang: "tc" });
    const r = await fetch(`/api/search/song?${params}`, { credentials: "same-origin", headers: { Accept: "application/json" } });
    // 202：驗證尚未完成，稍後再試
    if (r.status === 202 || r.headers.get("x-amzn-waf-action")) return false;
    if (!r.ok) return send({ error: `KKBOX 搜尋回應 HTTP ${r.status}` }), true;
    const v = await r.json();
    // 只保留需要的欄位，避免網址過長
    const result = (v?.data?.result || []).slice(0, MAX_RESULTS).map((x) => ({
      name: x?.name,
      url: x?.url,
      has_lyrics: x?.has_lyrics,
      album: { artist: { name: x?.album?.artist?.name } },
    }));
    send({ data: { data: { result } } });
    return true;
  }

  async function tick() {
    if (sent || busy) return;
    busy = true;
    try {
      if (await search()) return;
    } catch {
      // 驗證頁面或網路錯誤：稍後再試
    } finally {
      busy = false;
    }
    if (Date.now() - started > SHOW_AFTER_MS) requestShow();
    setTimeout(tick, RETRY_MS);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", tick, { once: true });
  } else {
    tick();
  }
})();
