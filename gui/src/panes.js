// Workspace pane controllers: the file rail (queued files + pipeline stages), the
// read-only Markdown review pane, and the center PDF preview. Each owns its own
// DOM subtree; none touch the transcript/progress UI (that lives in ui.js).

import { requireTauri } from "./tauri.js";
import { splitPath } from "./paths.js";
import { showToast, removeToast } from "./toasts.js";

/** Controller over the Workspace file-rail panes (EH-0004 bite 2): the file
 *  list and the OCR pipeline stages. Both are bound to live state, not static
 *  placeholders:
 *    - file list   <- the input queued for the next run (path field + Import btn)
 *    - pipeline    <- the preflight report (tools/model/mmproj flags) */
export function makeFileRail() {
  const count = document.getElementById("fileCount");
  const list = document.getElementById("fileList");
  const empty = document.getElementById("fileListEmpty");
  const engineLabel = document.getElementById("pipelineEngine");
  const stages = document.getElementById("pipelineStages");

  /** Render the file list from the queued input(s). Empty hides the row and
   *  shows the placeholder; one entry shows a single queued-file row. */
  function renderFiles(paths) {
    const items = (paths || []).map(splitPath).filter(Boolean);
    if (count) count.textContent = String(items.length);
    // Remove every previously rendered .file-row (keep the #fileListEmpty node).
    if (list) list.querySelectorAll(".file-row").forEach((n) => n.remove());
    if (items.length === 0) {
      if (empty) empty.hidden = false;
      return;
    }
    if (empty) empty.hidden = true;
    for (const item of items) {
      const row = document.createElement("div");
      row.className = "file-row";
      row.innerHTML =
        '<svg class="file-row__icon" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M3.5 2h6L13 5.5V14H3.5z"></path><path d="M9.5 2V5.5H13"></path></svg>' +
        '<div class="file-row__body">' +
        '<span class="file-row__name"></span>' +
        '<span class="file-row__path"></span>' +
        "</div>";
      row.querySelector(".file-row__name").textContent = item.name;
      row.querySelector(".file-row__path").textContent = item.path;
      if (list) list.appendChild(row);
    }
  }

  /** Render the OCR pipeline stages from a preflight report. Each stage lights
   *  up from the report's flags: tools/model/mmproj. ok => green, missing =>
   *  red, partial (build-too-old / only one tool) => amber. Unknown stays dim. */
  function renderPipeline(report) {
    if (engineLabel) {
      const quant = (report && report.quant) || "Q8_0";
      engineLabel.textContent = "Unlimited-OCR · " + quant;
    }
    if (!stages) return;
    const ok = !!(report && report.ok);
    const setStage = (key, state) => {
      const li = stages.querySelector('.stage[data-stage="' + key + '"]');
      if (!li) return;
      li.classList.remove("is-ok", "is-warn", "is-bad");
      if (state) li.classList.add("is-" + state);
    };

    // Tools: both llama-server and pdftoppm resolved on a successful report.
    if (!report) {
      setStage("tools", "");
    } else if (ok && report.llamaServer && report.pdftoppm) {
      // Soft warn if the build is known and below the min, even when ok.
      setStage("tools", "is-ok");
    } else {
      setStage("tools", "is-bad");
    }

    // Model GGUF + projector presence come straight off the report.
    setStage("model", report && report.modelPresent ? "is-ok" : report ? "is-bad" : "");
    setStage("mmproj", report && report.mmprojPresent ? "is-ok" : report ? "is-bad" : "");
  }

  return { renderFiles, renderPipeline };
}

/** Controller over the Markdown review/edit pane. The pane is the dedicated result
 *  surface: after a run (or a Library card click) it loads the written {stem}.md
 *  (via read_text_file) into an EasyMDE editor here. Edits Save back in place via the
 *  write_text_file command (same backend allowlist as read). Undo/redo + live preview
 *  come from EasyMDE; the preview HTML is sanitized with DOMPurify before display.
 *
 *  EasyMDE + DOMPurify are vendored UMD globals (index.html). Outside the webview or
 *  if EasyMDE failed to load, the pane falls back to the bare <textarea> so layout
 *  work still loads in a plain browser. */
export function makeMarkdownPane() {
  const el = document.getElementById("mdEditor");
  const source = document.getElementById("mdSource");
  const saveBtn = document.getElementById("mdSave");
  const exportSel = document.getElementById("mdExport");
  // The on-disk .md currently loaded. null = nothing loaded or an in-memory-only run
  // result (no file to overwrite); Save stays disabled until a real path is set.
  let currentPath = null;
  let editor = null;

  const PLACEHOLDER =
    "Select a file in the Library to view or edit it here, or run OCR to produce one.";

  if (el && typeof EasyMDE !== "undefined") {
    editor = new EasyMDE({
      element: el,
      // Default toolbar icons are FontAwesome glyphs auto-fetched from a CDN, which
      // the app CSP (style-src/font-src 'self') blocks and which break offline. Use a
      // text-label toolbar over EasyMDE's static actions instead — no FA dependency.
      autoDownloadFontAwesome: false,
      spellChecker: false,
      status: false,
      autosave: { enabled: false },
      placeholder: PLACEHOLDER,
      toolbar: [
        { name: "bold", action: EasyMDE.toggleBold, text: "B", title: "Bold" },
        { name: "italic", action: EasyMDE.toggleItalic, text: "I", title: "Italic" },
        { name: "heading", action: EasyMDE.toggleHeadingSmaller, text: "H", title: "Heading" },
        "|",
        { name: "quote", action: EasyMDE.toggleBlockquote, text: "❝", title: "Quote" },
        { name: "ul", action: EasyMDE.toggleUnorderedList, text: "•", title: "Bullet list" },
        { name: "ol", action: EasyMDE.toggleOrderedList, text: "1.", title: "Numbered list" },
        "|",
        { name: "link", action: EasyMDE.drawLink, text: "🔗", title: "Link" },
        "|",
        { name: "undo", action: EasyMDE.undo, text: "↶", title: "Undo" },
        { name: "redo", action: EasyMDE.redo, text: "↷", title: "Redo" },
        "|",
        { name: "preview", action: EasyMDE.togglePreview, text: "👁", title: "Toggle preview", noDisable: true },
        { name: "side", action: EasyMDE.toggleSideBySide, text: "⇔", title: "Side-by-side", noDisable: true },
      ],
      // Preview renders OCR output to HTML; sanitize so injected markup can't run.
      renderingConfig: {
        sanitizerFunction: (html) =>
          typeof DOMPurify !== "undefined" ? DOMPurify.sanitize(html) : html,
      },
    });
  }

  function setValue(markdown) {
    if (editor) editor.value(markdown || "");
    else if (el) el.value = markdown || "";
  }
  function getValue() {
    return editor ? editor.value() : el ? el.value : "";
  }

  /** Load fetched markdown + the on-disk path it came from. A path enables Save. */
  function render(markdown, path) {
    setValue(markdown);
    currentPath = path || null;
    if (source) {
      source.hidden = !currentPath;
      source.textContent = currentPath ? "source: " + currentPath : "";
    }
    if (saveBtn) saveBtn.disabled = !currentPath;
    if (exportSel) exportSel.disabled = !currentPath;
  }

  function clear() {
    setValue("");
    currentPath = null;
    if (source) source.hidden = true;
    if (saveBtn) saveBtn.disabled = true;
    if (exportSel) exportSel.disabled = true;
  }

  /** Overwrite the loaded .md on disk with the editor's current content. No-op when
   *  nothing is loaded. Fail-soft outside the webview; surfaces a toast either way. */
  async function save() {
    if (!currentPath) return;
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return;
    }
    try {
      await t.core.invoke("write_text_file", { path: currentPath, content: getValue() });
      showToast("md-save", { kind: "done", title: "Saved", meta: currentPath });
      removeToast("md-save", 2500);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[review] save failed:", err);
      showToast("md-save", { kind: "error", title: "Save failed", meta: String(err) });
    }
  }

  /** Export the loaded markdown to `format` via the export_markdown command (pandoc).
   *  Flushes the editor's current content to disk first so the exported document
   *  matches what's on screen (export reads the .md from disk). Surfaces toasts;
   *  fail-soft outside the webview. */
  async function exportAs(format) {
    if (!currentPath || !format) return;
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return;
    }
    showToast("md-export", { kind: "info", title: "Exporting…", meta: format.toUpperCase() });
    try {
      await t.core.invoke("write_text_file", { path: currentPath, content: getValue() });
      const out = await t.core.invoke("export_markdown", { srcPath: currentPath, format });
      showToast("md-export", { kind: "done", title: "Exported", meta: out });
      removeToast("md-export", 3500);
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[review] export failed:", err);
      showToast("md-export", { kind: "error", title: "Export failed", meta: String(err) });
    }
  }

  if (saveBtn) saveBtn.addEventListener("click", save);
  if (exportSel) {
    // The select acts as a menu: fire on pick, then reset to the "Export…" label.
    exportSel.addEventListener("change", () => {
      const format = exportSel.value;
      exportSel.value = "";
      exportAs(format);
    });
  }

  return { render, clear, save, exportAs };
}

/** Controller over the center PDF preview pane. Calls the `render_pages` command
 *  (cached on disk by the backend) and loads each page PNG through the asset
 *  protocol via convertFileSrc. Single image at a time; prev/next buttons and the
 *  Left/Right arrow keys page through, clamped to [0, n-1] (no wrap). Clicking the
 *  image advances one page. Fails soft outside the webview so layout work still
 *  loads in a plain browser. */
export function makePreviewPane() {
  const panel = document.querySelector(".panel.preview");
  if (!panel) return { show() {}, clear() {} };
  const stage = panel.querySelector(".preview__stage");
  const pageChip = panel.querySelector(".chip--soft");
  const pageCount = panel.querySelector(".preview__pagecount");
  const prevBtn = panel.querySelector("#prevPage");
  const nextBtn = panel.querySelector("#nextPage");
  // Lazy per-page model: render only the page being viewed (on demand, as the user
  // navigates), not every page up front. Importing a 300-page book no longer
  // rasterizes 300 PNGs when the user may never leave page 1. (No look-ahead
  // prefetch: each page is rendered when first shown, then cached.)
  let pdfPath = null; // current PDF; null when cleared
  let cache = {}; // 1-based page number -> asset URL (string), or null if out of range
  let idx = 1; // current page (1-based)
  let lastPage = null; // known last page once an out-of-range render is hit; null = unknown
  let token = 0; // bumps on each show()/clear() so stale async renders are dropped

  // Render+cache one page (1-based). Returns its asset URL, or null when the page is
  // out of range (the backend render_page Errs past the last page). Never throws.
  async function fetchPage(t, n) {
    if (n in cache) return cache[n];
    // Render the preview at the same DPI the run uses (#optDpi), not the backend's
    // fixed default, so what you preview matches what gets OCR'd. Invalid/blank ->
    // null lets the backend pick its default. ponytail: `cache` is keyed by page
    // only, so a DPI change after a page was viewed keeps the old image until clear;
    // key by `n+":"+dpi` if exact live re-render on DPI change is ever needed.
    const dpiEl = document.getElementById("optDpi");
    const dv = parseInt((dpiEl && dpiEl.value) || "", 10);
    const dpi = Number.isFinite(dv) && dv > 0 ? dv : null;
    try {
      const p = await t.core.invoke("render_page", { pdfPath, page: n, dpi });
      cache[n] = t.core.convertFileSrc(p);
    } catch (err) {
      // Only an out-of-range page marks the end of the document: cache the null and
      // bound navigation. A REAL render failure (pdftoppm error, transient IPC) is
      // NOT cached, so a later navigation retries instead of permanently truncating
      // the preview at a page that would render fine on a second try.
      if (String(err).includes("out of range")) {
        cache[n] = null;
        if (lastPage === null || n - 1 < lastPage) lastPage = Math.max(0, n - 1);
      } else {
        return undefined; // transient: leave uncached so the next attempt retries
      }
    }
    return cache[n];
  }

  function paint(errorMsg) {
    if (!stage) return;
    stage.innerHTML = "";
    const url = pdfPath ? cache[idx] : null;
    if (url == null) {
      const p = document.createElement("p");
      p.className = "placeholder";
      p.textContent = errorMsg || "Import a PDF to see a page preview here.";
      stage.appendChild(p);
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
    const url = await fetchPage(t, target);
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
    const url = await fetchPage(t, 1);
    if (my !== token) return; // superseded by a newer show()/clear()
    paint(url == null ? "Preview render failed." : undefined);
  }

  return { show, clear };
}
