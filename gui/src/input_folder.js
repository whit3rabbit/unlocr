// Input-folder dialog: stage folders/files client-side, then resolve them
// server-side via scan_input_paths (recursive walk with a depth cap) into a
// flat file list that gets appended to the shared queue. Wired once from
// main.js; the dialog owns its own staged-list state independent of `queue`
// until OK is clicked.

import { splitPath } from "./paths.js";
import { FILE_DIALOG_FILTERS } from "./formats.js";
import { showToast, removeToast } from "./toasts.js";

const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** Wire the "Input folder" trigger + dialog. `queue` is the shared file queue
 *  (main.js) -- the only thing this module calls on OK is `queue.add(...)`,
 *  so every existing subscriber (file rail, board, output autofill/gating)
 *  repaints automatically. */
export function wireInputFolderDialog(queue) {
  const btn = document.getElementById("importFolderBtn");
  const dialog = document.getElementById("inputFolderDialog");
  const addFolderBtn = document.getElementById("ifAddFolderBtn");
  const addFilesBtn = document.getElementById("ifAddFilesBtn");
  const recursiveCb = document.getElementById("ifRecursive");
  const listEl = document.getElementById("ifStagedList");
  const emptyEl = document.getElementById("ifStagedEmpty");
  const okBtn = document.getElementById("ifOkBtn");
  const scanningEl = document.getElementById("ifScanning");
  if (!btn || !dialog) return;

  let staged = []; // string[] of folder and/or file paths, client-side only

  /** Render the staged list using the file rail's own row markup
   *  (.file-row/.file-row__remove, file_rail.js) so the two lists match. */
  function renderStaged() {
    if (!listEl) return;
    listEl.querySelectorAll(".file-row").forEach((n) => n.remove());
    if (emptyEl) emptyEl.hidden = staged.length > 0;
    for (const full of staged) {
      const item = splitPath(full);
      if (!item) continue;
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
      const rm = document.createElement("button");
      rm.type = "button";
      rm.className = "file-row__remove";
      rm.textContent = "×";
      rm.setAttribute("aria-label", tr("action.remove") + " " + item.name);
      rm.addEventListener("click", () => {
        staged = staged.filter((p) => p !== full);
        renderStaged();
      });
      row.appendChild(rm);
      listEl.appendChild(row);
    }
  }

  function addStaged(paths) {
    for (const p of paths) if (!staged.includes(p)) staged.push(p);
    renderStaged();
  }

  btn.addEventListener("click", () => {
    staged = [];
    renderStaged();
    // Default OFF: matches the CLI's `--recursive` (opt-in, cli_args.rs).
    // Leave the checkbox at its unchecked markup state on every open.
    if (recursiveCb) recursiveCb.checked = false;
    dialog.showModal();
  });

  if (addFolderBtn) {
    addFolderBtn.addEventListener("click", async () => {
      const dlg = window.__TAURI__ && window.__TAURI__.dialog;
      if (!dlg || !dlg.open) return;
      const selected = await dlg.open({ directory: true, multiple: true });
      if (!selected) return;
      addStaged(Array.isArray(selected) ? selected : [selected]);
    });
  }

  if (addFilesBtn) {
    addFilesBtn.addEventListener("click", async () => {
      const dlg = window.__TAURI__ && window.__TAURI__.dialog;
      if (!dlg || !dlg.open) return;
      const selected = await dlg.open({
        multiple: true,
        directory: false,
        filters: FILE_DIALOG_FILTERS,
      });
      if (!selected) return;
      addStaged(Array.isArray(selected) ? selected : [selected]);
    });
  }

  if (okBtn) {
    okBtn.addEventListener("click", async () => {
      const t = window.__TAURI__;
      if (!t || !t.core || staged.length === 0) {
        dialog.close();
        return;
      }
      const recursive = !!(recursiveCb && recursiveCb.checked);
      if (scanningEl) scanningEl.hidden = false;
      okBtn.disabled = true;
      try {
        const resolved = await t.core.invoke("scan_input_paths", {
          paths: staged.slice(),
          recursive,
        });
        queue.add(resolved || []);
        dialog.close();
      } catch (err) {
        // eslint-disable-next-line no-console
        console.error("[input-folder] scan failed:", err);
        // Leave the dialog open so the user can see the staged list and retry
        // rather than silently losing their selection. Surface the failure
        // (e.g. a permissions error) as a toast, not just a console line, so
        // the user knows why nothing happened instead of it looking like a
        // no-op.
        showToast("input-folder-scan-error", {
          title: tr("inputFolder.scanError", { error: String(err) }),
          kind: "error",
        });
        removeToast("input-folder-scan-error", 6000);
      } finally {
        if (scanningEl) scanningEl.hidden = true;
        okBtn.disabled = false;
      }
    });
  }
}
