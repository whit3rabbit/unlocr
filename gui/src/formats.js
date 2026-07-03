// Single source of truth (on the JS side) for which raw image formats the OCR
// pipeline accepts directly, alongside PDF. Mirrors IMAGE_EXTENSIONS in
// src/inputs.rs -- keep both in sync when the accepted formats change.
export const IMAGE_EXTENSIONS = ["png", "jpg", "jpeg", "tiff", "tif", "webp", "bmp"];

/** True if `path`'s extension (case-insensitive) is a PDF or a recognized
 *  image format. Shared by drag-drop (run.js) and the Input Folder dialog's
 *  own "is this staged path acceptable" checks. */
export function isAcceptedInputPath(path) {
  const lower = String(path).toLowerCase();
  return lower.endsWith(".pdf") || IMAGE_EXTENSIONS.some((ext) => lower.endsWith("." + ext));
}

// Native file-picker filter list (tauri-plugin-dialog's `open({ filters })`
// shape), shared by every Import/Add-files dialog so the accepted extensions
// can't drift between call sites.
export const FILE_DIALOG_FILTERS = [
  { name: "PDF", extensions: ["pdf"] },
  { name: "Image", extensions: IMAGE_EXTENSIONS },
];
