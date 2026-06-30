# unlocr GUI

Desktop frontend for `unlocr` built with Tauri 2 and Vanilla HTML/JS.

## Keyboard Shortcuts (Accessibility & Productivity)

To make the app accessible to blind, visually impaired, and keyboard-only users, the following global keyboard shortcuts are supported throughout the application.

### Cross-Platform Keys Mapping
The app automatically adapts keyboard shortcuts to match standard operating system conventions:
*   **Command vs. Control**: Shortcuts using **`Cmd`** (Command key `⌘`) on macOS automatically translate to **`Ctrl`** (Control key) on Windows and Linux.
*   **Option vs. Alt**: Shortcuts using **`Alt`** on Windows and Linux map to the **`Option`** (⌥) key on macOS.

### View Navigation

Press these shortcuts (using **`Alt`** / **`Option`**) to switch between views instantly:

| Shortcut | Action | Description |
| :--- | :--- | :--- |
| **`Alt + W`** | Switch to **Workspace** | Focuses the main file input and control dashboard. |
| **`Alt + L`** | Switch to **Library** | Views past OCR jobs and history. |
| **`Alt + B`** | Switch to **Workflow Board** | Views active/completed runs in Kanban columns. |
| **`Alt + R`** | Switch to **Markdown Review** | Opens the markdown editor to edit completed runs. |
| **`Alt + S`** | Switch to **Settings** | Modifies model cache folders, custom configurations, and limits. |

### Document & Model Operations

These shortcuts execute actions directly. Note that native menu bar shortcuts use the standard platform modifier (`Ctrl` on Windows/Linux, `Cmd ⌘` on macOS).

| Shortcut | Menu Location | Action | Description |
| :--- | :--- | :--- | :--- |
| **`Cmd/Ctrl + O`** | `File > Load PDF...` | **Load PDF / Image** | Opens the native OS file picker to select a PDF/image. *(Note: **`Alt + I`** also works on Workspace/Board views).* |
| **`Cmd/Ctrl + M`** | `File > Load Model` | **Load Model** | Warm-loads the selected OCR GGUF/model. |
| **`Cmd/Ctrl + Shift + U`** | `File > Unload Model` | **Unload Model** | Unloads the model from memory to free system RAM. |
| **`Ctrl + Enter`** | *N/A* | **Run OCR** | Starts the OCR execution for queued files (Workspace/Board views). *(Note: **`Alt + Enter`** also works).* |
| **`Alt + M`** | *N/A* | **Focus Editor** | Shifts focus directly inside the Markdown text area (Markdown Review view). |
| **`Alt + N`** | *N/A* | **Toggle Notifications** | Opens/closes the notification panel dropdown in the top bar. |

*Note: Custom shortcuts containing modifier keys (`Alt` or `Ctrl`) work globally, even while typing inside text fields or editing markdown.*

---

## Build & Run

Ensure you are inside the `gui/` directory before running these commands:

### Development Mode
Runs the application with hot-reloading for frontend modifications:
```bash
cargo tauri dev
```

### Production Build
Bundles the application into native installers for your host OS (DMG/App on macOS, MSI/EXE on Windows, DEB/RPM on Linux):
```bash
cargo tauri build
```

### Syntax Validation
A fast syntax check for frontend JavaScript scripts:
```bash
node --check src/main.js
```
