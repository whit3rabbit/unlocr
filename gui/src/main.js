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
import {
  wireSettings,
  wireCacheControls,
  wireModelFilesTable,
  wireDependencies,
  wireSystemRequirements,
  wireAutoSaveEngineOptions,
} from "./settings.js";
import { initNotifications } from "./toasts.js";
import { wireQuickSettingsPopup } from "./quick_settings.js";
import { wireInputFolderDialog } from "./input_folder.js";
import { wirePageSelection, renderEffectiveSummary } from "./options.js";
import { preflightOnLoad } from "./ocr-events.js";
import { parentDirOf, splitPath } from "./paths.js";
import { FILE_DIALOG_FILTERS } from "./formats.js";

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
  wireRail(library, board, wireModelFilesTable);

  const ui = makeUi();
  const rail = makeFileRail();
  const mdPane = makeMarkdownPane();
  const unlistensRef = { value: [] };

  // EH-0015: wire the review-pane re-open affordance. Done job cards in the
  // Library become clickable; clicking switches to the Review view and loads
  // the card's .md. Rail buttons are needed so openInReview can update is-active.
  const railButtons = document.querySelectorAll(".rail__btn");
  library.setReviewHooks(mdPane, railButtons);
  // EH-0012 / bulk mode: canonical in-memory queue of PDF paths to process on Run.
  // The Import picker + typed path feed it; wireRunButton reads it via queue.get()
  // so all queued files run sequentially on one click. Subscribers (file rail,
  // board Queued column, output-field gating, path field) repaint on every change
  // via queue.onChange — registered below, after the helper defs they depend on.
  const queue = {
    paths: [],
    subs: [],
    get() {
      return this.paths.slice();
    },
    notify() {
      const p = this.get();
      this.subs.forEach((fn) => fn(p));
    },
    set(paths) {
      this.paths = (paths || []).filter((p) => typeof p === "string" && p.trim());
      this.notify();
    },
    add(paths) {
      const next = (paths || []).filter((p) => typeof p === "string" && p.trim());
      for (const p of next) if (!this.paths.includes(p)) this.paths.push(p);
      this.notify();
    },
    remove(path) {
      this.paths = this.paths.filter((p) => p !== path);
      this.notify();
    },
    clear() {
      this.set([]);
    },
    onChange(fn) {
      this.subs.push(fn);
    },
  };
  // Bind remove so it can be handed to the rail/board as a bare callback.
  queue.remove = queue.remove.bind(queue);
  wireRunButton(ui, mdPane, unlistensRef, () => queue.get());
  // EH-0006 bite 4: drag-drop PDF import onto the Library grid. Wired once on app
  // load; the listeners live for the app lifetime and are scoped to the Library
  // view inside the handler. Fail-soft outside the webview (plain browser).
  wireLibraryDrop(ui, mdPane, unlistensRef);
  wireInputFolderDialog(queue);

  // Backend-owned job lifecycle: run_ocr writes a `running` row when a file starts
  // and flips it to done/failed when it ends, emitting `jobs://changed` each time.
  // Reload the Library + Board so the Workflow board updates live (no tab switch).
  // App-lifetime listener (like the drag-drop ones), fail-soft outside the webview.
  const jobsEv = window.__TAURI__ && window.__TAURI__.event;
  if (jobsEv && jobsEv.listen) {
    jobsEv.listen("jobs://changed", () => {
      library.load();
      board.load();
    });
  }

  // Model load/remote wiring: engine tabs (local/remote), the Load/Unload bar,
  // the app-lifetime load-progress listeners, and the settings panel. Load
  // settings first so saved defaults seed the controls, then mark which quants are
  // cached, then read the live model status to set the Run gate + badge.
  wireEnginePreset();
  wireEngineDialog();
  wireModelBar(ui);
  attachLoadListeners();
  // markCachedQuants() doesn't depend on the settings load (list_local_models
  // is independent of the settings row), so it runs once here rather than
  // also inside wireSettings' onSaved -- that ran unconditionally right after
  // this call anyway, making the onSaved copy a redundant second IPC round trip.
  markCachedQuants();
  wireSettings().then(() => {
    // Wired only after wireSettings' restore has landed, so those initial
    // field assignments don't spuriously trigger an immediate re-save.
    wireAutoSaveEngineOptions();
  });
  wireCacheControls();
  // wireModelFilesTable is NOT called here: it sha256-hashes every cached GGUF
  // (multi-GB) and would peg a core at boot. wireRail runs it lazily on Settings-open.
  wireSystemRequirements();
  wireDependencies();
  wireQuickSettingsPopup();
  refreshModelStatus(ui);
  // The backend idle-unload watcher drops the warm model after N idle minutes and
  // emits model://unloaded; refresh the badge + Run gate so the UI reflects it.
  const unloadEv = window.__TAURI__ && window.__TAURI__.event;
  if (unloadEv && unloadEv.listen) {
    unloadEv.listen("model://unloaded", () => refreshModelStatus(ui));
  }
  // Retranslate the model status text + Local/Remote badge when the locale
  // changes (or first loads): both are set imperatively from tr() in model.js,
  // not data-i18n nodes, so without this they hold the launch-language strings.
  if (window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
    window.unlocrI18n.onLocaleChange(() => refreshModelStatus(ui));
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
  // Clicking the empty preview stage reuses the Import button's flow (native
  // picker -> queue.add -> preview.show, incl. the plain-browser focus fallback),
  // so there's a single import path. importBtn is assigned above, in scope here.
  const preview = makePreviewPane({
    onImportRequest: () => importBtn && importBtn.click(),
  });

  // Output filename is single-file only: enable it when exactly one PDF is queued,
  // otherwise disable + clear so a stale name can't apply to a batch (the backend
  // also rejects out_file with >1 input). Called on every queue change.
  const outFileEl = document.getElementById("outFile");
  const outFolderEl = document.getElementById("outFolder");
  function updateOutFileState() {
    if (!outFileEl) return;
    const single = queue.paths.length === 1;
    // pages mode writes a per-page folder named after the input stem; a custom
    // single filename is meaningless there. Disable + clear it, mirroring the
    // batch gate (the backend also ignores out_file for the folder name).
    const mode = (document.getElementById("optOutputMode") || {}).value || "single";
    const modeDisables = mode === "pages";
    outFileEl.disabled = !single || modeDisables;
    // Clear for batches/pages; also drop the autofill flag so a later single
    // selection re-fills cleanly (a user-typed name would already have cleared it).
    if (!single || modeDisables) {
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
    if (queue.paths.length !== 1) return;
    const path = queue.paths[0];
    autofill(outFolderEl, parentDirOf(path));
    autofill(outFileEl, mdName(path));
  }

  // Single source of truth: every queue change repaints the file rail (each row
  // gets a remove × that drops the exact path), the board Queued column, the
  // output-field gating, and the path field. Registered as queue.onChange below.
  function syncQueueUi(paths) {
    rail.renderFiles(paths, queue.remove);
    board.renderPending();
    // Show the lone file in the path field for context; blank for 0/2+ (the rail
    // and board show the full list).
    if (pathInput) pathInput.value = paths.length === 1 ? paths[0] : "";
    updateOutFileState();
    autofillOutputs();
    // Drop the preview if its file was removed; advance to the next if any remain.
    preview.sync(paths);
  }
  queue.onChange(syncQueueUi);
  // Bulk mode: the board's Queued column mirrors the in-memory queue; a Remove on a
  // board card drops the same path the rail's × would.
  board.bindQueue(() => queue.get(), queue.remove);

  // Board head controls (bulk mode): Add PDFs reuses the workspace Import picker
  // (which appends to the queue); Run all reuses the workspace Run button (batch
  // run, respects the model-loaded gate). Both delegate to the existing buttons so
  // there is one code path for import and run.
  const runBtn = document.getElementById("runBtn");
  // A run consumes the pending queue: wireRunButton captured the paths synchronously
  // on click, so clearing here (a later-registered listener on the same click) only
  // empties the pending cards — they reappear as real Running/Done rows via the
  // store + jobs://changed.
  if (runBtn) runBtn.addEventListener("click", () => queue.clear());
  const boardAddBtn = document.getElementById("boardAddBtn");
  if (boardAddBtn) boardAddBtn.addEventListener("click", () => importBtn && importBtn.click());
  const boardRunBtn = document.getElementById("boardRunBtn");
  if (boardRunBtn && runBtn) boardRunBtn.addEventListener("click", () => runBtn.click());

  if (pathInput) {
    const syncFromField = () => {
      const v = (pathInput.value || "").trim();
      // Typed path replaces the entire queue (single-file typed entry). queue.set
      // notifies syncQueueUi, which repaints the rail/board/output fields.
      queue.set(v ? [v] : []);
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
            filters: FILE_DIALOG_FILTERS,
          });
          // selected is null (cancelled), string (single), or string[] (multiple).
          if (!selected) return;
          const picked = Array.isArray(selected) ? selected : [selected];
          const pdfs = picked.filter((p) => typeof p === "string" && p.trim());
          if (pdfs.length === 0) return;
          // Bulk mode: append (don't replace) so successive Imports build one batch.
          queue.add(pdfs);
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

  // Output mode dropdown lives in #outputOpts (outside #runOpts, so the listener
  // above does not cover it): refresh the summary AND re-evaluate the out-file
  // gate (pages mode disables/clears the filename field) on every change.
  const outputModeEl = document.getElementById("optOutputMode");
  if (outputModeEl) {
    outputModeEl.addEventListener("change", () => {
      updateOutFileState();
      renderEffectiveSummary();
    });
  }

  // Task preset + Prompt box: the Task select picks the prompt actually sent; the
  // Prompt box is an optional verbatim override (empty -> the Task preset). No autofill
  // wiring needed -- options.js resolves the override and the #runOpts listener above
  // refreshes the effective-values summary when either changes.

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

  // The pipeline pane (preflight-derived) must not stay frozen at boot state.
  // Re-run preflight when readiness can change: quant selection (checks a
  // different GGUF) or a model load/unload (a download may have added the GGUF).
  document.getElementById("optQuant")?.addEventListener("change", () => preflightOnLoad(ui, rail));
  document.addEventListener("unlocr:model-changed", () => preflightOnLoad(ui, rail));

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

  // Accessibility global hotkeys
  if (typeof hotkeys !== "undefined") {
    // Inside an input/textarea/contenteditable, only honor the submit-style
    // modifiers (Ctrl/Cmd) so the Run shortcut (ctrl+enter / cmd+enter) fires
    // there too, including macOS Cmd. Alt is excluded in editables so Alt+Enter
    // stays a normal key (newline / IME), not a hijacked Run trigger; Alt-based
    // shortcuts still work when focus is not in a text field (return true).
    hotkeys.filter = function(event) {
      const target = event.target || event.srcElement;
      const isEditable = target.tagName === "INPUT" || target.tagName === "TEXTAREA" || (target && target.isContentEditable);
      if (isEditable) {
        return event.ctrlKey || event.metaKey;
      }
      return true;
    };

    // 1. Navigation Rail Switching (Alt + W/L/B/R/S)
    const routes = {
      "alt+w": "workspace",
      "alt+l": "library",
      "alt+b": "board",
      "alt+r": "review",
      "alt+s": "settings",
    };
    Object.keys(routes).forEach((key) => {
      hotkeys(key, (event) => {
        event.preventDefault();
        const btn = document.querySelector(`.rail__btn[data-route="${routes[key]}"]`);
        if (btn) btn.click();
      });
    });

    // 2. Import PDF (Alt + I)
    hotkeys("alt+i", (event) => {
      const activeView = document.querySelector(".view.is-shown");
      if (activeView && (activeView.dataset.view === "workspace" || activeView.dataset.view === "board")) {
        event.preventDefault();
        const importBtn = document.getElementById("importBtn");
        if (importBtn && !importBtn.disabled) importBtn.click();
      }
    });

    // 3. Run OCR (Ctrl+Enter, Cmd+Enter, or Alt+Enter)
    hotkeys("alt+enter,ctrl+enter,cmd+enter", (event) => {
      const activeView = document.querySelector(".view.is-shown");
      if (activeView && (activeView.dataset.view === "workspace" || activeView.dataset.view === "board")) {
        event.preventDefault();
        const runBtn = document.getElementById("runBtn");
        if (runBtn && !runBtn.disabled) runBtn.click();
      }
    });

    // 4. Toggle Notifications Panel (Alt + N)
    hotkeys("alt+n", (event) => {
      event.preventDefault();
      const bell = document.getElementById("notifyBell");
      if (bell) bell.click();
    });

    // 5. Focus Markdown editor on Review view (Alt + M)
    hotkeys("alt+m", (event) => {
      const activeView = document.querySelector(".view.is-shown");
      if (activeView && activeView.dataset.view === "review") {
        event.preventDefault();
        if (mdPane && typeof mdPane.focus === "function") {
          mdPane.focus();
        }
      }
    });
  }
});
