import { requireTauri } from "./tauri.js";
import { fmtBytes } from "./toasts.js";

// window.unlocrI18n loads before this module (classic <script> in <head>); fall
// back to identity so a plain-browser/no-i18n context still renders something.
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** Controller over the center PDF preview pane. Calls the `render_pages` command
 *  (cached on disk by the backend) and loads each page PNG through the asset
 *  protocol via convertFileSrc. Single image at a time; prev/next buttons and the
 *  Left/Right arrow keys page through, clamped to [0, n-1] (no wrap). Clicking the
 *  image advances one page. Clicking the empty stage (no PDF loaded) requests an
 *  import via onImportRequest; it is a no-op once a PDF is loaded so it never
 *  competes with click-to-advance. Fails soft outside the webview so layout work
 *  still loads in a plain browser. */
export function makePreviewPane(handlers = {}) {
  const { onImportRequest } = handlers;
  const panel = document.querySelector(".panel.preview");
  if (!panel) return { show() {}, clear() {}, sync() {} };
  const stage = panel.querySelector(".preview__stage");
  const pageChip = panel.querySelector(".chip--soft");
  const pageCount = panel.querySelector(".preview__pagecount");
  const prevBtn = panel.querySelector("#prevPage");
  const nextBtn = panel.querySelector("#nextPage");
  const infoBtn = panel.querySelector("#pdfInfoBtn");
  // The info dialog lives at the end of <body> (native <dialog>, like
  // #engineDialog), not inside .panel.preview.
  const infoDlg = document.getElementById("pdfInfoDialog");
  const infoBody = document.getElementById("pdfInfoBody");
  // Lazy per-page model: render only the page being viewed (on demand, as the user
  // navigates), not every page up front. Importing a 300-page book no longer
  // rasterizes 300 PNGs when the user may never leave page 1. (No look-ahead
  // prefetch: each page is rendered when first shown, then cached.)
  let pdfPath = null; // current PDF; null when cleared
  let cache = {}; // "n:dpi" -> asset URL (string), or null if out of range
  let idx = 1; // current page (1-based)
  let lastPage = null; // known last page once an out-of-range render is hit; null = unknown
  let token = 0; // bumps on each show()/clear() so stale async renders are dropped

  // Render+cache one page (1-based). Returns its asset URL, or null when the page is
  // out of range (the backend render_page Errs past the last page). Never throws.
  async function fetchPage(t, n, dpi) {
    const key = n + ":" + dpi;
    if (key in cache) return cache[key];
    try {
      const p = await t.core.invoke("render_page", { pdfPath, page: n, dpi });
      cache[key] = t.core.convertFileSrc(p);
    } catch (err) {
      // Only an out-of-range page marks the end of the document: cache the null and
      // bound navigation. A REAL render failure (pdftoppm error, transient IPC) is
      // NOT cached, so a later navigation retries instead of permanently truncating
      // the preview at a page that would render fine on a second try.
      if (String(err).includes("out of range")) {
        cache[key] = null;
        if (lastPage === null || n - 1 < lastPage) lastPage = Math.max(0, n - 1);
      } else {
        return undefined; // transient: leave uncached so the next attempt retries
      }
    }
    return cache[key];
  }

  function paint(errorMsg) {
    if (!stage) return;
    if (infoBtn) infoBtn.disabled = !pdfPath;
    stage.innerHTML = "";
    const dpiEl = document.getElementById("optDpi");
    const dv = parseInt((dpiEl && dpiEl.value) || "", 10);
    const dpi = Number.isFinite(dv) && dv > 0 ? dv : null;
    const url = pdfPath ? cache[idx + ":" + dpi] : null;
    if (url == null) {
      const p = document.createElement("p");
      p.className = "placeholder";
      p.textContent = errorMsg || "Import a PDF to see a page preview here.";
      stage.appendChild(p);
      // Empty stage is the click-to-import target: signal it. Cleared on the
      // image branch so a loaded page uses the img's own (click-to-advance) cursor.
      stage.style.cursor = onImportRequest ? "pointer" : "";
      if (pageChip) pageChip.textContent = "Page 0";
      if (pageCount) pageCount.textContent = "page 0 / 0";
      if (prevBtn) prevBtn.disabled = true;
      if (nextBtn) nextBtn.disabled = true;
      return;
    }
    const img = document.createElement("img");
    img.className = "preview__img";
    img.src = url;
    img.alt = "PDF page " + idx;
    // Click advances when a next page may exist (unknown end, or before the last).
    if (lastPage === null || idx < lastPage) {
      img.title = "click for next page";
      img.style.cursor = "pointer";
      img.addEventListener("click", () => go(1));
    }
    stage.appendChild(img);
    stage.style.cursor = ""; // the img sets its own cursor (pointer when clickable)
    if (pageChip) pageChip.textContent = "Page " + idx;
    // Total is unknown until the user reaches the end (no separate page-count probe);
    // show "page N" until then, "page N / total" once discovered.
    if (pageCount) {
      pageCount.textContent = lastPage !== null ? "page " + idx + " / " + lastPage : "page " + idx;
    }
    if (prevBtn) prevBtn.disabled = idx <= 1;
    if (nextBtn) nextBtn.disabled = lastPage !== null && idx >= lastPage;
  }

  // Move by delta (1-based, no wrap), rendering the target page on demand.
  async function go(delta) {
    if (!pdfPath) return;
    const target = idx + delta;
    if (target < 1) return;
    if (lastPage !== null && target > lastPage) return;
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return;
    }
    const my = token;
    const dpiEl = document.getElementById("optDpi");
    const dv = parseInt((dpiEl && dpiEl.value) || "", 10);
    const dpi = Number.isFinite(dv) && dv > 0 ? dv : null;
    const url = await fetchPage(t, target, dpi);
    if (my !== token) return; // a newer show()/clear() superseded this render
    if (url == null) {
      paint(); // hit the end; nextBtn disables via lastPage
      return;
    }
    idx = target;
    paint();
  }

  if (prevBtn) prevBtn.addEventListener("click", () => go(-1));
  if (nextBtn) nextBtn.addEventListener("click", () => go(1));

  function addInfoRow(label, value) {
    if (value === null || value === undefined || value === "") return;
    const row = document.createElement("div");
    row.className = "pdf-info__row";
    const l = document.createElement("span");
    l.className = "pdf-info__label";
    l.textContent = label;
    const v = document.createElement("span");
    v.className = "pdf-info__value";
    v.textContent = String(value);
    row.append(l, v);
    infoBody.appendChild(row);
  }

  function renderInfo(info) {
    infoBody.innerHTML = "";
    addInfoRow(tr("pdfInfo.fieldPages"), info.pages);
    addInfoRow(tr("pdfInfo.fieldTitle"), info.title);
    addInfoRow(tr("pdfInfo.fieldAuthor"), info.author);
    addInfoRow(tr("pdfInfo.fieldProducer"), info.producer);
    addInfoRow(tr("pdfInfo.fieldCreated"), info.creationDate);
    addInfoRow(tr("pdfInfo.fieldPageSize"), info.pageSize);
    addInfoRow(tr("pdfInfo.fieldVersion"), info.pdfVersion);
    addInfoRow(tr("pdfInfo.fieldFileSize"), fmtBytes(info.fileSizeBytes));
    if (info.encrypted !== null && info.encrypted !== undefined) {
      addInfoRow(
        tr("pdfInfo.fieldEncrypted"),
        info.encrypted ? tr("pdfInfo.yes") : tr("pdfInfo.no"),
      );
    }
  }

  // Info popup for the PDF currently shown in this pane (the closure's own
  // `pdfPath`, NOT the #pdfPath text input -- a multi-file import shows the
  // first picked PDF without updating that input, so it can point at the
  // wrong file). Disabled via paint() while pdfPath is null.
  if (infoBtn && infoDlg && infoBody && typeof infoDlg.showModal === "function") {
    infoBtn.addEventListener("click", async () => {
      if (!pdfPath) return;
      let t;
      try {
        t = requireTauri();
      } catch (err) {
        return;
      }
      infoBody.innerHTML = "";
      infoDlg.showModal();
      try {
        const info = await t.core.invoke("pdf_info", { pdfPath });
        renderInfo(info);
      } catch (err) {
        infoBody.innerHTML = "";
        const p = document.createElement("p");
        p.className = "placeholder";
        p.textContent = String(err);
        infoBody.appendChild(p);
      }
    });
  }
  // Empty-stage click-to-import: the stage is stable across paint() (the
  // placeholder <p> is rebuilt each render, so the listener must live on the
  // stage). No-op once a PDF is loaded; a click on the rendered image bubbles
  // here but pdfPath is set, so it never reopens the picker over advance-on-click.
  if (stage && onImportRequest) {
    stage.addEventListener("click", () => {
      if (pdfPath) return;
      onImportRequest();
    });
  }
  // Arrow keys page when the preview pane has focus or hover (don't hijack typing
  // in an input/textarea elsewhere).
  document.addEventListener("keydown", (e) => {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    const t = e.target;
    if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT")) return;
    if (!panel.matches(":hover")) return;
    go(e.key === "ArrowRight" ? 1 : -1);
  });

  function clear() {
    token++;
    pdfPath = null;
    cache = {};
    idx = 1;
    lastPage = null;
    paint();
  }

  /** Reconcile the preview with the queue after a change (called by main.js on
   *  every queue.onChange). If the file currently being previewed was removed,
   *  advance to the first remaining file, or clear when the queue is now empty.
   *  Removing a non-previewed file leaves the current page untouched; a no-op when
   *  nothing is previewed yet (never auto-previews just because the queue grew). */
  function sync(paths) {
    if (!pdfPath) return;
    const list = (paths || []).filter((p) => typeof p === "string" && p.trim());
    if (list.includes(pdfPath)) return; // current file still queued: leave the page
    if (list.length === 0) clear();
    else show(list[0]); // previewed file was removed: show the first remaining
  }

  /** Render the first page of one PDF; later pages load lazily on navigation. Non-PDF
   *  or empty clears the pane. Fails soft outside the webview. */
  async function show(path) {
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return; // plain browser: no backend render
    }
    token++;
    const my = token;
    if (!path || !path.toLowerCase().endsWith(".pdf")) {
      clear();
      return;
    }
    pdfPath = path;
    cache = {};
    idx = 1;
    lastPage = null;
    const dpiEl = document.getElementById("optDpi");
    const dv = parseInt((dpiEl && dpiEl.value) || "", 10);
    const dpi = Number.isFinite(dv) && dv > 0 ? dv : null;
    const url = await fetchPage(t, 1, dpi);
    if (my !== token) return; // superseded by a newer show()/clear()
    paint(url == null ? "Preview render failed." : undefined);
  }

  return { show, clear, sync };
}
