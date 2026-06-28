// unlocr shell wiring — boot entry point.
//
// The implementation is split into ES modules (loaded via <script type="module">,
// no bundler, see CLAUDE.md): tauri.js (bridge), paths.js (path/time helpers),
// toasts.js (notifications), panes.js (file rail / markdown / preview), jobs.js
// (Library / Board / rail nav / record), ui.js (transcript+progress controller),
// options.js (engine options form), ocr-events.js (ocr:// subscription +
// preflight), run.js (run flow + drag-drop), model.js (model bar + presets),
// settings.js (Settings panel + cache). This file only wires them on DOMContentLoaded.

import { makeLibrary, makeBoard, wireRail } from "./jobs.js";
import { makeUi } from "./ui.js";
import { makeFileRail, makeMarkdownPane, makePreviewPane } from "./panes.js";
import { wireRunButton, wireLibraryDrop } from "./run.js";
import {
  wireEnginePreset,
  wireEngineDialog,
  wireModelBar,
  attachLoadListeners,
  markCachedQuants,
  refreshModelStatus,
} from "./model.js";
import { wireSettings, wireCacheControls } from "./settings.js";
import { initNotifications } from "./toasts.js";
import { wirePageSelection, renderEffectiveSummary } from "./options.js";
import { preflightOnLoad } from "./ocr-events.js";
import { parentDirOf, splitPath } from "./paths.js";

// Derive the default output filename for a single PDF: <stem>.md (mirrors the
// backend's blank-filename default). Strips the last extension off the basename.
function mdName(path) {
  const r = splitPath(path);
  if (!r) return "";
  return r.name.replace(/\.[^.]+$/, "") + ".md";
}

window.addEventListener("DOMContentLoaded", () => {
  const library = makeLibrary();
  const board = makeBoard();
  wireRail(library, board);

  const ui = makeUi();
  const rail = makeFileRail();
  const mdPane = makeMarkdownPane();
  const unlistensRef = { value: [] };

  // EH-0015: wire the review-pane re-open affordance. Done job cards in the
  // Library become clickable; clicking switches to the Review view and loads
  // the card's .md. Rail buttons are needed so openInReview can update is-active.
  const railButtons = document.querySelectorAll(".rail__btn");
  library.setReviewHooks(mdPane, railButtons);
  // EH-0012: canonical queue of PDF paths to process on Run. The Import picker
  // populates this; the path-input field seeds it too (single-file typed entry).
  // wireRunButton reads this via getQueuedPaths() so all imported files run
  // sequentially on one click instead of only the last typed path.
  let queuedPaths = [];
  const getQueuedPaths = () => queuedPaths.slice();
  wireRunButton(ui, mdPane, unlistensRef, library, board, getQueuedPaths);
  // EH-0006 bite 4: drag-drop PDF import onto the Library grid. Wired once on app
  // load; the listeners live for the app lifetime and are scoped to the Library
  // view inside the handler. Fail-soft outside the webview (plain browser).
  wireLibraryDrop(ui, mdPane, unlistensRef, library, board);

  // Model load/remote wiring: engine tabs (local/remote), the Load/Unload bar,
  // the app-lifetime load-progress listeners, and the settings panel. Load
  // settings first so saved defaults seed the controls, then mark which quants are
  // cached, then read the live model status to set the Run gate + badge.
  wireEnginePreset();
  wireEngineDialog();
  wireModelBar(ui);
  attachLoadListeners();
  wireSettings(() => {
    markCachedQuants();
  });
  markCachedQuants();
  wireCacheControls();
  refreshModelStatus(ui);
  // The backend idle-unload watcher drops the warm model after N idle minutes and
  // emits model://unloaded; refresh the badge + Run gate so the UI reflects it.
  const unloadEv = window.__TAURI__ && window.__TAURI__.event;
  if (unloadEv && unloadEv.listen) {
    unloadEv.listen("model://unloaded", () => refreshModelStatus(ui));
  }
  // Notification bell + panel + download toasts. Self-contained; silent in a
  // plain browser (no Tauri). Seeds the unread badge from the persisted store.
  initNotifications();

  // Native File menu (lib.rs) emits one event per action; reuse the existing
  // toolbar buttons by id so all their validation/status logic is shared.
  // Unload is disabled when no model is loaded, so the guard makes it a no-op.
  const menuEv = window.__TAURI__ && window.__TAURI__.event;
  if (menuEv && menuEv.listen) {
    const menuMap = {
      menu_load_pdf: "importBtn",
      menu_load_model: "loadModelBtn",
      menu_unload_model: "unloadModelBtn",
    };
    menuEv.listen("menu://action", (e) => {
      const btn = document.getElementById(menuMap[e.payload]);
      if (btn && !btn.disabled) btn.click();
    });
  }

  // EH-0004 bite 2 / EH-0012: the file list pane is bound to the queued-path
  // list. The Import button opens a MULTI-select picker; each chosen PDF is
  // added to queuedPaths and rendered in the file-rail. The path-input field
  // provides single-file typed/pasted entry (adds one path on change). The Run
  // button processes queuedPaths in order, with per-file status.
  const pathInput = document.getElementById("pdfPath");
  const importBtn = document.getElementById("importBtn");
  const preview = makePreviewPane();

  // Output filename is single-file only: enable it when exactly one PDF is queued,
  // otherwise disable + clear so a stale name can't apply to a batch (the backend
  // also rejects out_file with >1 input). Called on every queue change.
  const outFileEl = document.getElementById("outFile");
  const outFolderEl = document.getElementById("outFolder");
  function updateOutFileState() {
    if (!outFileEl) return;
    const single = queuedPaths.length === 1;
    outFileEl.disabled = !single;
    // Clear for batches; also drop the autofill flag so a later single selection
    // re-fills cleanly (a user-typed name would already have cleared the flag).
    if (!single) {
      outFileEl.value = "";
      delete outFileEl.dataset.autofilled;
    }
  }

  // Autofill a field with a default WITHOUT clobbering a user-typed value: write
  // only when the field is empty or still holds a previous autofill (data-autofilled).
  // A keystroke in the field clears the flag (see listeners below), so once the user
  // edits it we never overwrite. Lets folder/filename follow the selected PDF until
  // the user takes ownership of the value.
  function autofill(el, value) {
    if (!el || el.disabled) return;
    const owned = el.value && !el.dataset.autofilled;
    if (owned) return;
    el.value = value;
    if (value) el.dataset.autofilled = "1";
    else delete el.dataset.autofilled;
  }
  [outFolderEl, outFileEl].forEach((el) => {
    if (el) el.addEventListener("input", () => delete el.dataset.autofilled);
  });

  // Prefill the output folder + filename from the single queued PDF (folder = its
  // directory, filename = <stem>.md, matching the backend's blank defaults). No-op
  // for 0/2+ files: folder is left as-is, filename is gated by updateOutFileState.
  function autofillOutputs() {
    if (queuedPaths.length !== 1) return;
    const path = queuedPaths[0];
    autofill(outFolderEl, parentDirOf(path));
    autofill(outFileEl, mdName(path));
  }

  // Apply queuedPaths to the file-rail display and clear the text field
  // (the canonical list is in queuedPaths, not the field, for multi-file batches).
  function applyQueue(paths) {
    queuedPaths = paths.slice();
    rail.renderFiles(queuedPaths);
    // Show the first file in the path field for context; for multi-file batches
    // this is the first item only (the rail shows the full list).
    if (pathInput) pathInput.value = queuedPaths.length === 1 ? queuedPaths[0] : "";
    updateOutFileState();
    autofillOutputs();
  }

  if (pathInput) {
    const syncFromField = () => {
      const v = (pathInput.value || "").trim();
      // Typed path replaces the entire queue (single-file typed entry).
      queuedPaths = v ? [v] : [];
      rail.renderFiles(queuedPaths);
      updateOutFileState();
      autofillOutputs();
    };
    // Preview render shells out to pdftoppm; only refresh on blur/Enter/change,
    // not per keystroke.
    const refreshPreview = () => preview.show((pathInput.value || "").trim());
    pathInput.addEventListener("input", syncFromField);
    pathInput.addEventListener("change", syncFromField);
    pathInput.addEventListener("change", refreshPreview);

    // Import opens the native multi-select file picker (tauri-plugin-dialog,
    // exposed at window.__TAURI__.dialog via withGlobalTauri). The picker result
    // is a string (single) or string[] (multiple) for multi:true.
    // EH-0012: `multiple: true` so the user can pick several PDFs at once; all
    // are added to queuedPaths and shown in the file-rail.
    if (importBtn) {
      importBtn.addEventListener("click", async () => {
        const dialog = window.__TAURI__ && window.__TAURI__.dialog;
        if (!dialog || !dialog.open) {
          pathInput.focus();
          return;
        }
        try {
          const selected = await dialog.open({
            multiple: true,
            directory: false,
            filters: [{ name: "PDF", extensions: ["pdf"] }],
          });
          // selected is null (cancelled), string (single), or string[] (multiple).
          if (!selected) return;
          const picked = Array.isArray(selected) ? selected : [selected];
          const pdfs = picked.filter((p) => typeof p === "string" && p.trim());
          if (pdfs.length === 0) return;
          applyQueue(pdfs);
          // Preview the first file; multi-file batches show page 1 of the first PDF.
          preview.show(pdfs[0]);
        } catch (err) {
          // eslint-disable-next-line no-console
          console.warn("[import] picker failed:", err.message);
          pathInput.focus();
        }
      });
    }
  }

  // Output-folder picker: native folder dialog (same plugin as the PDF/GGUF
  // pickers). Sets the #outFolder field; blank field = write beside the input.
  const outFolderBtn = document.getElementById("outFolderBtn");
  if (outFolderBtn && outFolderEl) {
    outFolderBtn.addEventListener("click", async () => {
      const dialog = window.__TAURI__ && window.__TAURI__.dialog;
      if (!dialog || !dialog.open) {
        outFolderEl.focus();
        return;
      }
      try {
        const dir = await dialog.open({ directory: true, multiple: false });
        // A picked folder is a deliberate user choice: set it and drop the autofill
        // flag so a later PDF selection does not overwrite it.
        if (typeof dir === "string" && dir.trim()) {
          outFolderEl.value = dir;
          delete outFolderEl.dataset.autofilled;
        }
      } catch (err) {
        // eslint-disable-next-line no-console
        console.warn("[output] folder picker failed:", err.message);
      }
    });
  }

  // Start empty (matches the "No files imported yet" placeholder).
  rail.renderFiles([]);
  updateOutFileState();

  // EH-0005 bite 2: the "effective values" summary mirrors whatever the engine
  // options controls hold, so it never drifts from the next Run's payload. Update
  // it on every change of any control (input/change covers select, number,
  // checkbox, and textarea) and once on load for the correct first paint.
  const optsControls = document.querySelectorAll(
    "#runOpts input, #runOpts select, #runOpts textarea, #optKeepImages"
  );
  optsControls.forEach((el) => {
    el.addEventListener("input", renderEffectiveSummary);
    el.addEventListener("change", renderEffectiveSummary);
  });

  // Task preset -> fill the Prompt box. Keep these strings in sync with the CLI's
  // Task::prompt() (src/main.rs). "custom" leaves whatever the user typed.
  const TASK_PROMPTS = {
    markdown: "<|grounding|>Convert the document to markdown.",
    free: "Free OCR.",
    figure: "Parse the figure.",
  };
  const taskEl = document.getElementById("optTask");
  const promptEl = document.getElementById("optPrompt");
  if (taskEl && promptEl) {
    taskEl.addEventListener("change", () => {
      const preset = TASK_PROMPTS[taskEl.value];
      if (preset) {
        promptEl.value = preset;
        renderEffectiveSummary();
      }
    });
    // A manual prompt edit means the box no longer matches a preset: flip to Custom
    // so the dropdown does not falsely claim a preset is active.
    promptEl.addEventListener("input", () => {
      const match = Object.keys(TASK_PROMPTS).find((k) => TASK_PROMPTS[k] === promptEl.value);
      taskEl.value = match || "custom";
    });
  }

  // Surface the Q4_K_M loop caveat only when that quant is selected.
  const quantEl = document.getElementById("optQuant");
  const quantHint = document.getElementById("quantHint");
  if (quantEl && quantHint) {
    const syncHint = () => {
      quantHint.hidden = quantEl.value !== "Q4_K_M";
    };
    quantEl.addEventListener("change", syncHint);
    syncHint();
  }

  // Page-selection mode -> show/hide the from/to inputs.
  wirePageSelection();

  renderEffectiveSummary();

  // Preflight only runs inside the Tauri webview; fail soft otherwise so the
  // static page still loads in a plain browser (e.g. for layout work). Passing
  // `ui` turns preflight into the Run-gate (EH-0004 bite 1): a missing tool
  // disables Run and shows the reason inline. Passing `rail` (bite 2) drives the
  // pipeline stages from the same report.
  try {
    preflightOnLoad(ui, rail);
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[preflight] skipped:", err.message);
  }

  // EH-0006: load the persisted job store on startup so the Library grid (bite 2)
  // and the Board columns (bite 3) show prior runs immediately (both are reloaded on
  // Run + on tab switch too). Fail soft outside the webview (plain browser) so layout
  // work still loads.
  try {
    library.load();
    board.load();
  } catch (err) {
    // eslint-disable-next-line no-console
    console.warn("[store] load skipped:", err.message);
  }
});
