// Engine/options form (EH-0005): reads the run controls into the run_ocr payload,
// reads the Pages selector, toggles the page-number inputs, and renders the
// "effective values" summary next to Run. Pure DOM; the summary reads from the
// same readRunOptions() the Run button uses so it can never drift from the payload.

/** Read the engine/options controls (EH-0005 bites 1 + 2) into the run_ocr invoke
 *  payload. Every control defaults to unlocr::OcrOptions::default() in the markup
 *  (quant=Q8_0, dpi=144, max_tokens=4096, keep_images=false, prompt="<|grounding|>
 *  Convert the document to markdown."), so a user who touches nothing sends
 *  CLI-parity values. Invalid/empty numbers fall back to the same defaults so a
 *  half-typed field never crashes a run. An empty prompt also falls back to the
 *  default so a run never sends a blank prompt. Keys are camelCase to match the
 *  Tauri command's parameter names. */
export function readRunOptions() {
  const quantEl = document.getElementById("optQuant");
  const dpiEl = document.getElementById("optDpi");
  const maxTokensEl = document.getElementById("optMaxTokens");
  const keepImagesEl = document.getElementById("optKeepImages");
  const promptEl = document.getElementById("optPrompt");
  const repeatPenaltyEl = document.getElementById("optRepeatPenalty");

  const DEFAULT_DPI = 144;
  const DEFAULT_MAX_TOKENS = 4096;
  const DEFAULT_PROMPT = "<|grounding|>Convert the document to markdown.";
  const numOr = (el, fallback) => {
    const v = parseInt((el && el.value) || "", 10);
    return Number.isFinite(v) && v > 0 ? v : fallback;
  };
  // Optional float; blank/invalid -> null so the backend omits it (server default).
  const floatOrNull = (el) => {
    const v = parseFloat((el && el.value) || "");
    return Number.isFinite(v) && v > 0 ? v : null;
  };
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
    prompt: promptOr(promptEl, DEFAULT_PROMPT),
    repeatPenalty: floatOrNull(repeatPenaltyEl),
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

/** Show/hide the page-number inputs based on the Pages mode and keep the second
 *  input (the range "to") visible only for Range. Called once at startup and on
 *  every mode change so the form matches the selected mode. */
export function wirePageSelection() {
  const modeEl = document.getElementById("optPagesMode");
  const wrap = document.getElementById("optPagesInputs");
  const toEl = document.getElementById("optPageTo");
  const label = document.getElementById("optPagesInputsLabel");
  const fromEl = document.getElementById("optPageFrom");
  if (!modeEl || !wrap) return;
  const apply = () => {
    const mode = modeEl.value;
    wrap.hidden = mode === "all";
    if (toEl) toEl.hidden = mode !== "range";
    if (label) label.textContent = mode === "range" ? "Pages" : "Page";
    if (fromEl) fromEl.placeholder = mode === "range" ? "from" : "page";
    renderEffectiveSummary();
  };
  // Visibility toggle on mode change; the summary is refreshed by the shared
  // #runOpts input/change listeners wired in DOMContentLoaded.
  modeEl.addEventListener("change", apply);
  apply();
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
    if (opts.lastPage != null && opts.lastPage !== f) pagesNote = " · pages " + f + "-" + opts.lastPage;
    else if (opts.lastPage == null) pagesNote = " · pages " + f + "-end";
    else pagesNote = " · page " + f;
  }
  const modeNote = opts.outputMode === "single" ? "" : " · output " + opts.outputMode;
  vals.textContent =
    opts.quant +
    " · " + opts.dpi + " DPI · " + opts.maxTokens + " tok · " +
    "keep images " + (opts.keepImages ? "on" : "off") +
    pagesNote +
    modeNote +
    " · prompt: “" + shown + ellipsis + "”";
}
