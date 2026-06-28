// Path + timestamp helpers shared across the file rail, job cards, and run flow.
// Pure (no DOM, no Tauri), so the self-check at the bottom can exercise them on
// load. Handles both `/` and `\` so Windows paths render the right basename.

/** Split a path into a display name + the full path for a queued-file row.
 *  Handles both `/` and `\` so Windows paths (C:\Users\me\file.pdf) show the
 *  right basename. Returns null for an empty/whitespace-only string so callers
 *  can filter(Boolean). */
export function splitPath(path) {
  const clean = (path || "").trim();
  if (!clean) return null;
  const sep = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  const name = sep >= 0 ? clean.slice(sep + 1) : clean;
  return { name: name || clean, path: clean };
}

/** Derive the parent directory of a path ("" if none). Used to pick the out_dir
 *  for a run so the {stem}.md is written beside the source PDF (mirrors the CLI's
 *  "output alongside input" intent) and the returned path is real. Handles both
 *  `/` and `\` so a Windows path (C:\Users\me\file.pdf) yields C:\Users\me, not
 *  "" (which would silently fall into in-memory mode and write no file). */
export function parentDirOf(p) {
  const clean = (p || "").trim();
  if (!clean) return "";
  const sep = Math.max(clean.lastIndexOf("/"), clean.lastIndexOf("\\"));
  if (sep < 0) return ""; // bare filename: no parent
  if (sep === 0) return clean.slice(0, 1); // POSIX root: "/a.pdf" -> "/"
  const dir = clean.slice(0, sep);
  // Windows drive root: "C:\a.pdf" -> "C:" is drive-RELATIVE; join would resolve
  // against drive C's CWD, not the root. Return "C:\" so the path stays absolute.
  if (/^[A-Za-z]:$/.test(dir)) return dir + "\\";
  return dir;
}

/** EH-0006: pull the basename off a POSIX or Windows path for a job card title.
 *  Delegates to splitPath. Returns "(untitled run)" for a missing input path so a
 *  card never renders an empty title. */
export function jobBaseName(path) {
  const r = splitPath(path);
  return r ? r.name : "(untitled run)";
}

/** Format a unix epoch-seconds value as a short local timestamp for a card footer.
 *  Falls back to the raw number if the browser cannot parse it so the value is
 *  never lost. */
export function formatEpoch(secs) {
  const n = Number(secs);
  if (!Number.isFinite(n) || n <= 0) {
    if (secs !== undefined && secs !== null && secs !== "") {
      // eslint-disable-next-line no-console
      console.error("[paths] formatEpoch: bad unix epoch value:", secs);
    }
    return String(secs);
  }
  const d = new Date(n * 1000);
  if (Number.isNaN(d.getTime())) return String(secs);
  // YYYY-MM-DD HH:MM in local time, compact and locale-stable.
  const pad = (x) => String(x).padStart(2, "0");
  return (
    d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) +
    " " + pad(d.getHours()) + ":" + pad(d.getMinutes())
  );
}

// Self-check: POSIX + Windows path splitting. Cheap, runs once on load; throws
// loudly in the console if the separator logic regresses. (no test framework here)
(function selfCheckPaths() {
  // parentDirOf: returns the parent directory, empty string when no parent.
  const dirCases = [
    ["/home/me/a.pdf", "/home/me"],
    ["/a.pdf", "/"],
    ["a.pdf", ""],
    ["C:\\Users\\me\\a.pdf", "C:\\Users\\me"],
    ["C:\\a.pdf", "C:\\"],
  ];
  for (const [input, want] of dirCases) {
    const got = parentDirOf(input);
    if (got !== want) {
      // eslint-disable-next-line no-console
      console.error(`parentDirOf(${input}) = ${got}, want ${want}`);
    }
  }

  // splitPath: returns { name, path } where name is the basename (cross-platform).
  const splitCases = [
    ["/home/me/a.pdf",         "a.pdf"],
    ["/a.pdf",                 "a.pdf"],
    ["a.pdf",                  "a.pdf"],
    ["C:\\Users\\me\\a.pdf",   "a.pdf"],
    ["C:\\a.pdf",              "a.pdf"],
  ];
  for (const [input, wantName] of splitCases) {
    const r = splitPath(input);
    const gotName = r && r.name;
    if (gotName !== wantName) {
      // eslint-disable-next-line no-console
      console.error(`splitPath(${input}).name = ${gotName}, want ${wantName}`);
    }
    if (r && r.path !== input.trim()) {
      // eslint-disable-next-line no-console
      console.error(`splitPath(${input}).path = ${r.path}, want ${input.trim()}`);
    }
  }
  // Empty / whitespace-only input must return null (filtered by callers).
  if (splitPath("") !== null || splitPath("  ") !== null) {
    // eslint-disable-next-line no-console
    console.error("splitPath should return null for empty/whitespace input");
  }
})();
