// Password handling for encrypted PDFs. Two surfaces:
//   1. Interactive single-PDF prompt (detect via pdf_needs_password, validate via
//      check_pdf_password, re-prompt on a wrong password, Cancel skips the file).
//   2. A bulk password file (one password per line) for batches whose PDFs use
//      different passwords; each PDF is tried against the loaded lines.
// Resolved passwords are session-only, held in memory here, and NEVER persisted
// (not written to the settings DB, not logged).

import { requireTauri } from "./tauri.js";

const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

// path -> validated password string. A file the user already unlocked this session
// is not re-prompted (the run flow and the preview pane share this cache).
const sessionPw = new Map();

// Candidate passwords from the bulk file picker (trimmed, blank/# lines dropped by
// the backend read_password_file command). Empty = no bulk file loaded.
let bulkPasswords = [];

/** Open the password dialog, resolving to the typed string, or null on Cancel/ESC.
 *  `error` (when set) shows an inline "wrong password" message from the prior try. */
export function promptPassword(t, { error } = {}) {
  const dlg = document.getElementById("pdfPasswordDialog");
  const input = document.getElementById("pdfPasswordInput");
  const errEl = document.getElementById("pdfPasswordError");
  if (!dlg || typeof dlg.showModal !== "function" || !input) {
    return Promise.resolve(null);
  }
  return new Promise((resolve) => {
    input.value = "";
    if (errEl) {
      errEl.textContent = error || "";
      errEl.hidden = !error;
    }
    // The input is not inside a <form>, so Enter would do nothing; wire it to submit
    // as OK. ESC / the × / Cancel resolve to null (returnValue defaults to "").
    dlg.returnValue = "";
    const onKey = (e) => {
      if (e.key === "Enter") {
        e.preventDefault();
        dlg.close("ok");
      }
    };
    const onClose = () => {
      dlg.removeEventListener("close", onClose);
      input.removeEventListener("keydown", onKey);
      resolve(dlg.returnValue === "ok" ? input.value : null);
    };
    input.addEventListener("keydown", onKey);
    dlg.addEventListener("close", onClose);
    dlg.showModal();
    input.focus();
  });
}

/** Resolve the password to use for one PDF `path`, interactively if needed.
 *  Returns `{ password, cancelled }`: `password` is null when no password is needed,
 *  a string when one unlocks it; `cancelled` is true only when the user dismissed the
 *  prompt (the caller should skip that file). Uses, in order: the session cache, the
 *  loaded bulk file, then the prompt loop. Non-PDF paths short-circuit to no password. */
export async function ensurePasswordFor(t, path) {
  if (!path || !path.toLowerCase().endsWith(".pdf")) {
    return { password: null, cancelled: false };
  }
  if (sessionPw.has(path)) {
    return { password: sessionPw.get(path), cancelled: false };
  }
  let needs = false;
  try {
    needs = await t.core.invoke("pdf_needs_password", { pdfPath: path });
  } catch (_e) {
    needs = false; // probe failed (e.g. no pdftoppm): let the run surface the real error
  }
  if (!needs) return { password: null, cancelled: false };

  // Try the bulk file's candidates before prompting, so a loaded file unlocks a
  // batch without a dialog per PDF.
  for (const pw of bulkPasswords) {
    try {
      if (await t.core.invoke("check_pdf_password", { pdfPath: path, password: pw })) {
        sessionPw.set(path, pw);
        return { password: pw, cancelled: false };
      }
    } catch (_e) {
      /* keep trying the next candidate */
    }
  }

  let error = null;
  for (;;) {
    const pw = await promptPassword(t, { error });
    if (pw == null) return { password: null, cancelled: true };
    let ok = false;
    try {
      ok = await t.core.invoke("check_pdf_password", { pdfPath: path, password: pw });
    } catch (_e) {
      ok = false;
    }
    if (ok) {
      sessionPw.set(path, pw);
      return { password: pw, cancelled: false };
    }
    error = tr("pdfPassword.wrong");
  }
}

/** The session password already validated for `path`, or null. Used by the preview
 *  pane so an unlocked PDF renders without re-prompting. */
export function sessionPasswordFor(path) {
  return (path && sessionPw.get(path)) || null;
}

/** Wire the bulk password-file picker (button + path label + Clear). The native
 *  file picker is opened on the BACKEND (pick_password_file), which reads the chosen
 *  file and returns the candidate list: the renderer never supplies a path, so it
 *  cannot widen the backend read scope to an arbitrary file. */
export function wirePasswordFilePicker() {
  let t;
  try {
    t = requireTauri();
  } catch (_e) {
    return; // plain browser: no native picker
  }
  const btn = document.getElementById("optPasswordFileBtn");
  const label = document.getElementById("optPasswordFilePath");
  const clearBtn = document.getElementById("optPasswordFileClear");
  if (!btn) return;

  const setLabel = (text) => {
    if (label) label.textContent = text;
  };

  btn.addEventListener("click", async () => {
    let lines;
    try {
      lines = await t.core.invoke("pick_password_file");
    } catch (err) {
      bulkPasswords = [];
      setLabel(String(err));
      return;
    }
    if (lines == null) return; // user cancelled the picker
    bulkPasswords = Array.isArray(lines) ? lines : [];
    setLabel(tr("opts.passwordFileLoaded", { count: bulkPasswords.length }));
  });

  if (clearBtn) {
    clearBtn.addEventListener("click", () => {
      bulkPasswords = [];
      setLabel(tr("opts.passwordFileNone"));
    });
  }
}
