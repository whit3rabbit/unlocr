// Engine/options form (EH-0005): reads the run controls into the run_ocr payload,
// reads the Pages selector, toggles the page-number inputs, and renders the
// "effective values" summary next to Run. Pure DOM; the summary reads from the
// same readRunOptions() the Run button uses so it can never drift from the payload.

// Task preset -> the user prompt actually sent. Unlimited-OCR uses NO system prompt;
// the model needs a user-role task instruction (`<image>` is injected by llama.cpp's
// mtmd from the image part, NOT this text). `<|grounding|>` lives only on the grounding
// preset (markdown + layout coordinates); the default `markdown` preset emits clean
// markdown with no boxes. The Prompt box overrides this when non-empty. Keep in sync
// with the CLI's Task::prompt() (src/cli_args.rs).
export const TASK_PROMPTS = {
  markdown: "document parsing.",
  grounding: "<|grounding|>Convert the document to markdown.",
  free: "Free OCR.",
  figure: "Parse the figure.",
};

// EH-0013 bite 2: i18n hook. Named `tr` -- `t` is the range "to" page number in
// readPageSelection(). TASK_PROMPTS above are model-instruction payloads and stay
// canonical English on purpose (they are sent to the model, not shown to the user).
const tr = (window.unlocrI18n && window.unlocrI18n.t) || ((k) => k);

/** Parse a positive integer from a control, falling back when blank/invalid.
 *  Shared by readRunOptions() and settings.js's auto-save (so both agree on
 *  what counts as a valid override). */
export const numOr = (el, fallback) => {
  const v = parseInt((el && el.value) || "", 10);
  return Number.isFinite(v) && v > 0 ? v : fallback;
};

/** Optional float; blank/invalid -> null so the backend omits it (server default). */
export const floatOrNull = (el) => {
  const v = parseFloat((el && el.value) || "");
  return Number.isFinite(v) && v > 0 ? v : null;
};

/** Same, but 0 is a real value (DRY: an explicit 0 means "off" and must reach
 *  the backend, or the local-path 1.0 default would override it). */
export const floatOrNullMin0 = (el) => {
  const v = parseFloat((el && el.value) || "");
  return Number.isFinite(v) && v >= 0 ? v : null;
};

/** Assign `document.getElementById(id).value = v`, but only when both the
 *  element exists and `v` isn't null/undefined (preserves a legitimate 0/false
 *  rather than skipping it). Shared by every settings restore/save-feedback
 *  site (settings.js, quick_settings.js) so the guard can't drift between them. */
export const setVal = (id, v) => {
  const el = document.getElementById(id);
  if (el != null && v != null) el.value = v;
};

/** Read the engine/options controls (EH-0005 bites 1 + 2) into the run_ocr invoke
 *  payload. Every control defaults to unlocr::OcrOptions::default() in the markup
 *  (quant=Q8_0, dpi=144, max_tokens=4096, keep_images=false), so a user who touches
 *  nothing sends CLI-parity values. Invalid/empty numbers fall back to the same
 *  defaults so a half-typed field never crashes a run. The Prompt box is an OPTIONAL
 *  override: when empty it falls back to the selected Task preset, so a run never
 *  sends a blank prompt. Keys are camelCase to match the Tauri command's parameter
 *  names. */
export function readRunOptions() {
  const quantEl = document.getElementById("optQuant");
  const dpiEl = document.getElementById("optDpi");
  const maxTokensEl = document.getElementById("optMaxTokens");
  const keepImagesEl = document.getElementById("optKeepImages");
  const promptEl = document.getElementById("optPrompt");
  const taskEl = document.getElementById("optTask");
  const temperatureEl = document.getElementById("optTemperature");
  const repeatPenaltyEl = document.getElementById("optRepeatPenalty");
  const dryMultiplierEl = document.getElementById("optDryMultiplier");
  const dryBaseEl = document.getElementById("optDryBase");

  const DEFAULT_DPI = 144;
  const DEFAULT_MAX_TOKENS = 4096;
  // Empty Prompt box -> the selected Task preset (markdown if the select is missing).
  const taskPrompt = TASK_PROMPTS[taskEl && taskEl.value] || TASK_PROMPTS.markdown;
  const promptOr = (el, fallback) => {
    const v = (el && el.value) || "";
    // trim only newlines/whitespace at the ends; an interior-only edit is real.
    const trimmed = v.replace(/^\s+|\s+$/g, "");
    return trimmed.length > 0 ? trimmed : fallback;
  };

  const pages = readPageSelection();

  return {
    quant: (quantEl && quantEl.value) || "Q8_0",
    dpi: numOr(dpiEl, DEFAULT_DPI),
    maxTokens: numOr(maxTokensEl, DEFAULT_MAX_TOKENS),
    keepImages: !!(keepImagesEl && keepImagesEl.checked),
    prompt: promptOr(promptEl, taskPrompt),
    // 0 is the meaningful default (deterministic OCR), so use the DRY-style
    // "0 is real" parse rather than repeatPenalty's "blank/0 -> null".
    temperature: floatOrNullMin0(temperatureEl),
    repeatPenalty: floatOrNull(repeatPenaltyEl),
    dryMultiplier: floatOrNullMin0(dryMultiplierEl),
    // No "0 = off" meaning for a DRY base; blank/invalid/0 -> null (server default).
    dryBase: floatOrNull(dryBaseEl),
    // 1-based inclusive page range; both null = all pages. The backend validates
    // first>=1 and last>=first (a direct invoke bypasses the form min= clamp).
    firstPage: pages.firstPage,
    lastPage: pages.lastPage,
    // single/pages/both; backend parse_output_mode validates (defaults to single).
    outputMode: (document.getElementById("optOutputMode") || {}).value || "single",
  };
}

/** Read the Pages control (All / Range / Single) into a `{firstPage, lastPage}`
 *  pair. All (or a blank/invalid entry) -> both null (= all pages). Single -> the
 *  same page for both bounds. Range -> the two bounds, leaving a blank bound null
 *  for the backend to default (first->1, last->first). Never throws on a half-typed
 *  field: an unparseable number falls back to all so a run is never blocked. */
export function readPageSelection() {
  const mode = (document.getElementById("optPagesMode") || {}).value || "all";
  const posInt = (id) => {
    const v = parseInt((document.getElementById(id) || {}).value || "", 10);
    return Number.isFinite(v) && v > 0 ? v : null;
  };
  if (mode === "single") {
    const n = posInt("optPageFrom");
    return n === null ? { firstPage: null, lastPage: null } : { firstPage: n, lastPage: n };
  }
  if (mode === "range") {
    const f = posInt("optPageFrom");
    const t = posInt("optPageTo");
    if (f === null && t === null) return { firstPage: null, lastPage: null };
    return { firstPage: f, lastPage: t };
  }
  return { firstPage: null, lastPage: null };
}

/** Show/hide the page-number inputs to match #optPagesMode's current value, and
 *  keep the second input (the range "to") visible only for Range. Exported (not
 *  just an inline wirePageSelection closure) so a caller that needs to re-run
 *  this after restoring settings (settings.js's applySettingsToControls) can
 *  call it directly instead of dispatching a synthetic `change` event on
 *  #optPagesMode -- that event is also observed by wireAutoSaveEngineOptions,
 *  and a synthetic dispatch would spuriously re-trigger an auto-save. */
export function applyPageSelectionVisibility() {
  const modeEl = document.getElementById("optPagesMode");
  const wrap = document.getElementById("optPagesInputs");
  const toEl = document.getElementById("optPageTo");
  const label = document.getElementById("optPagesInputsLabel");
  const fromEl = document.getElementById("optPageFrom");
  if (!modeEl || !wrap) return;
  const mode = modeEl.value;
  wrap.hidden = mode === "all";
  if (toEl) toEl.hidden = mode !== "range";
  if (label) label.textContent = mode === "range" ? tr("opts.pages") : tr("opts.pageSingular");
  if (fromEl) fromEl.placeholder = mode === "range" ? tr("opts.from") : tr("opts.pagePh");
  renderEffectiveSummary();
}

/** Wire the Pages mode select so a user's mode change re-runs the visibility
 *  toggle above. Called once at startup. */
export function wirePageSelection() {
  const modeEl = document.getElementById("optPagesMode");
  if (!modeEl) return;
  // Visibility toggle on mode change; the summary is refreshed by the shared
  // #runOpts input/change listeners wired in DOMContentLoaded.
  modeEl.addEventListener("change", applyPageSelectionVisibility);
  applyPageSelectionVisibility();
}

/** Render the "effective values" summary (EH-0005 bite 2) next to Run so the user
 *  sees what the next run will send before clicking. Reads from the same
 *  readRunOptions() the Run button uses, so it can never drift from the payload.
 *  The prompt is summarized (not shown verbatim) to keep the line scannable: the
 *  first sentence after any grounding tag, truncated. Empty/missing nodes fall
 *  back to the defaults so the summary is correct on first paint. */
export function renderEffectiveSummary() {
  const vals = document.getElementById("effectiveVals");
  if (!vals) return;
  const opts = readRunOptions();
  // Pull the human-readable tail of the prompt (strip a leading <|tag|>) and cap
  // it so a long custom prompt does not blow out the one-line summary.
  const promptShort = opts.prompt
    .replace(/^<\|[^|]*\|\>/, "")
    .replace(/^\s+|\s+$/g, "");
  const shown = promptShort.length > 0 ? promptShort.slice(0, 48) : opts.prompt.slice(0, 48);
  const ellipsis = promptShort.length > 48 ? "…" : "";
  // Page span: blank = all. "5" for single, "5-9" for range, "5-end" when only the
  // first bound is set. Mirrors the firstPage/lastPage the run will send.
  let pagesNote = "";
  if (opts.firstPage != null || opts.lastPage != null) {
    const f = opts.firstPage != null ? opts.firstPage : 1;
    if (opts.lastPage != null && opts.lastPage !== f) pagesNote = tr("eff.pagesRange", { from: f, to: opts.lastPage });
    else if (opts.lastPage == null) pagesNote = tr("eff.pagesToEnd", { from: f });
    else pagesNote = tr("eff.pageSingle", { n: f });
  }
  const modeNote = opts.outputMode === "single" ? "" : tr("eff.output", { mode: opts.outputMode });
  vals.textContent = tr("eff.summary", {
    quant: opts.quant,
    dpi: opts.dpi,
    maxTokens: opts.maxTokens,
    keepImages: tr(opts.keepImages ? "job.on" : "job.off"),
    pages: pagesNote,
    mode: modeNote,
    prompt: shown + ellipsis,
  });
}

// EH-0013: re-render the effective-values summary on a locale switch so its
// composed string (DPI/tok/keep-images/prompt) retranslates instantly.
if (typeof window !== "undefined" && window.unlocrI18n && window.unlocrI18n.onLocaleChange) {
  window.unlocrI18n.onLocaleChange(renderEffectiveSummary);
}
