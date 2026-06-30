import { requireTauri } from "./tauri.js";
import { showToast, removeToast } from "./toasts.js";

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
      // text-label toolbar over EasyMDE's static actions instead; no FA dependency.
      autoDownloadFontAwesome: false,
      spellChecker: false,
      status: false,
      autosave: { enabled: false },
      placeholder: PLACEHOLDER,
      // Change CodeMirror input style to contenteditable to make it accessible to
      // screen readers and OS selection (similar design paradigm to CodeMirror 6).
      codeMirrorOptions: {
        inputStyle: "contenteditable"
      },
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

    // Post-process the toolbar buttons to assign explicit aria-label values for screen readers
    const toolbarEl = el.parentNode ? el.parentNode.querySelector(".editor-toolbar") : document.querySelector(".editor-toolbar");
    if (toolbarEl) {
      toolbarEl.querySelectorAll("button").forEach((btn) => {
        const title = btn.getAttribute("title");
        if (title) {
          btn.setAttribute("aria-label", title);
        }
      });
    }
  }

  function setValue(markdown) {
    if (editor) editor.value(markdown || "");
    else if (el) el.value = markdown || "";
  }
  function getValue() {
    return editor ? editor.value() : el ? el.value : "";
  }

  /** Load fetched markdown + the on-disk path it came from. A real .md path
   *  enables Save/Export; a non-.md path (e.g. the output folder passed in pages
   *  mode) is shown for context but leaves Save/Export disabled (no single file
   *  to overwrite or hand to pandoc). */
  function render(markdown, path) {
    setValue(markdown);
    const isMd = !!path && /\.md$/i.test(path);
    currentPath = isMd ? path : null;
    if (source) {
      source.hidden = !path;
      source.textContent = path
        ? isMd
          ? "source: " + path
          : "saved to: " + path
        : "";
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

  /** Export the on-disk markdown to `format` via the export_markdown command
   *  (pandoc). Exports what was last SAVED to disk, NOT the live editor buffer:
   *  there is no hidden save here, so experimental edits are not silently written
   *  as a side effect of Export. Save first to export unsaved edits. Surfaces
   *  toasts; fail-soft outside the webview. */
  async function exportAs(format, retried = false) {
    if (!currentPath || !format) return;
    let t;
    try {
      t = requireTauri();
    } catch (err) {
      return;
    }
    showToast("md-export", { kind: "info", title: "Exporting…", meta: format.toUpperCase() });
    try {
      const out = await t.core.invoke("export_markdown", { srcPath: currentPath, format });
      showToast("md-export", { kind: "done", title: "Exported", meta: out });
      removeToast("md-export", 3500);
    } catch (err) {
      // Pandoc missing: on a platform that can auto-download (Windows), offer to fetch
      // it and retry ONCE. The `retried` guard prevents an unbounded download loop if
      // the download "succeeds" but pandoc is still not resolvable (extracted to a dir
      // preflight::locate does not scan); after one retry, fall through to the error.
      if (!retried && String(err).includes("pandoc not found") && (await offerPandocDownload(t))) {
        await exportAs(format, true);
        return;
      }
      // eslint-disable-next-line no-console
      console.error("[review] export failed:", err);
      showToast("md-export", { kind: "error", title: "Export failed", meta: String(err) });
    }
  }

  /** When pandoc is missing, ask to download it (Windows). Returns true if it was
   *  fetched (caller retries the export). False if declined, unavailable, or failed. */
  async function offerPandocDownload(t) {
    const dialog = window.__TAURI__ && window.__TAURI__.dialog;
    if (!dialog || typeof dialog.ask !== "function") return false;
    let ok = false;
    try {
      ok = await dialog.ask("pandoc is required to export and was not found. Download it now?", {
        title: "unlocr",
        kind: "info",
      });
    } catch (err) {
      return false;
    }
    if (!ok) return false;
    showToast("md-export", { kind: "info", title: "Downloading pandoc…" });
    try {
      await t.core.invoke("download_tool", { name: "pandoc" });
      return true;
    } catch (err) {
      // eslint-disable-next-line no-console
      console.error("[review] pandoc download failed:", err);
      showToast("md-export", { kind: "error", title: "pandoc download failed", meta: String(err) });
      return false;
    }
  }

  function focus() {
    if (editor && typeof editor.codemirror === "object" && typeof editor.codemirror.focus === "function") {
      editor.codemirror.focus();
    } else if (el && typeof el.focus === "function") {
      el.focus();
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

  return { render, clear, save, exportAs, focus };
}
