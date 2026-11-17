# Premiere Spotlight (Rust + Slint + Adobe UXP)

An ultra-lightweight, zero-WebView Spotlight launcher for Adobe Premiere Pro effects and project media.

## Architecture

1. **`helper-app/` (Rust + Slint Software Renderer)**
   - **Single process, zero WebView2:** Compiles `.slint` declarative UI directly to native Rust machine code rendered via Slint's CPU software renderer (`renderer-software`).
   - **Global Hotkey (`Ctrl + Space`):** Toggles the frameless centered floating window (`620×440`).
   - **Fuzzy Search (`nucleo-matcher`):** Fuzzy, typo-tolerant search across Video Effects, Audio Effects, Transitions, and custom presets. Exact, prefix and whole-word name matches rank first, then folder, bin and kind matches (type `transition` or a bin name). Effects you applied most this session get a capped boost.
   - **Localhost WebSocket Bridge (`ws://127.0.0.1:3847`):** Event-driven, 0% idle CPU bridge to Premiere Pro.

2. **`uxp-plugin/` (Adobe Premiere Pro UXP Plugin)**
   - Connects to `ws://127.0.0.1:3847` when Premiere Pro starts.
   - Automatically scans all installed Video Effects (`VideoFilterFactory`), Audio Effects (`AudioFilterFactory`), and Transitions (`TransitionFactory`) and syncs them to the Rust helper.
   - Applies the chosen effect to all selected timeline clips inside a single undoable Premiere Pro transaction.

## Quick Start

### 1. Run the Rust + Slint Floating Spotlight App
```powershell
cd helper-app
cargo run --release
```
Start Premiere Pro first: Spotlight exits at once when Premiere isn't running. Each launch also writes the bundled bridge plugin into `%APPDATA%\Adobe\UXP\Plugins\External` and enables it in Premiere's plugin list.

### 2. Load the Premiere Pro UXP Plugin
1. Open the **Adobe UXP Developer Tool** (or pack `uxp-plugin/` as a `.ccx` ZIP archive).
2. Click **Add Plugin** and select `uxp-plugin\manifest.json` in this repository.
3. With Adobe Premiere Pro open, click **Load** on **Premiere Spotlight Bridge**.

### 3. Build the Windows Installer (`.exe`)
Requires [Inno Setup 6](https://jrsoftware.org/isinfo.php) (`winget install --id JRSoftware.InnoSetup -e`).
```powershell
powershell -ExecutionPolicy Bypass -File .\installer\build-installer.ps1
```
This builds `helper-app` in release mode and outputs `dist\PremiereSpotlightSetup-<version>.exe`, which installs Premiere Spotlight per-user into `%LOCALAPPDATA%\Programs\Premiere Spotlight`, registers the UXP bridge plugin, and adds a Windows Installed Apps uninstaller. Spotlight does not start with Windows: the bridge plugin launches it while Premiere Pro is open, and it exits when Premiere closes.

## Development

### Project layout
- `helper-app/src/main.rs`: command-line flags, the single-instance check and startup.
- `helper-app/src/app/`: the palette app. `startup.rs` wires the window, bridge and hooks to the shared state; `results`, `palette`, `activation` and `requests` hold the search list, window, result activation and Premiere replies.
- `helper-app/src/bridge.rs`: the WebSocket server and the connected plugins.
- `helper-app/src/protocol.rs`: the JSON messages exchanged with the plugin.
- `helper-app/src/premiere_prefs.rs`: reads Premiere's preferences and keyboard shortcut profiles.
- `helper-app/src/win_focus/`: Windows integration (processes, foreground, synthetic input, window style and placement).
- `helper-app/ui/`: Slint windows. `spotlight.slint` is the file `build.rs` compiles.
- `uxp-plugin/index.js`: the bridge connection and caches. `lib/` and `actions/` hold the Premiere commands; they receive Premiere's API as a parameter and keep no state.
- `installer/`: the Inno Setup script and build script.

Every shipped file under `uxp-plugin/` (tests and `package.json` excluded) is embedded into the helper at build time, so new plugin modules need no installer change.

### Running tests
```powershell
cd helper-app
cargo test --profile quick
```
```powershell
cd uxp-plugin
npm test
```
CI runs both suites and Clippy on Windows for every push to `main` and every pull request.

## Keyboard Controls (Inside Spotlight Popup)
- **Type:** Fuzzy search effects, transitions, custom presets, project items, and actions in **All**. Selected presets and project items show their folder path.
- **`↑` / `↓` Arrows:** Navigate results
- **`Ctrl+P` (configurable):** Play or pause the highlighted audio clip in Project or All. The highlighted audio row also has a play/pause icon. During preview, click the progress line to seek within the clip’s Premiere In and Out points. Audio preview requires Premiere Pro 25.6 or later and supports WAV, PCM AIFF, MP3, FLAC, AAC, and AAC/ALAC in M4A.
- **`Tab`:** Cycle through **All** → **FX** → **PRJ** → **ACT**, or click a tab below the search bar to go straight to that catalog. Spotlight opens in **All** and shows recently applied effects and inserted items from the active project. Select a recent item to apply or insert it again.
- **Add Marker (All or ACT):** Add a sequence marker at the playhead and open Premiere’s Marker dialog. At an existing marker, open its dialog. Uses Premiere’s Timeline and Add Marker shortcuts.
- **Add Named Marker (All or ACT):** The search box becomes a name field. Type a name and press **Enter** to add a marker with that name at the playhead in one undo step, without the Marker dialog. Leave the field empty for an unnamed marker, or press **Esc** to go back to the search. Pause playback first. If a marker already exists at the playhead, Spotlight leaves it alone and says so. Assign a hotkey under **Hotkeys** to open Spotlight straight into the name field.
- **Go to Marker (All or ACT):** Search markers across the active project by marker name, sequence name, comment, or elapsed position. Each row shows its sequence. Choose a marker to open its sequence and move the playhead to its start, or press **Esc** to return to the catalog you opened it from.
- **Add Adjustment Layer (All or ACT):** Select one or more video clips to place a single adjustment layer across their full span. Spotlight uses the first available video track above the selection and adds a top track if needed. To set up a project, create an adjustment layer in Premiere, place and select it on the timeline, then run **Register Adjustment Layer**. You can remove that timeline instance after registering. Requires Premiere Pro 25.6 or later.
- **`Enter` (or Click) on `Remove Attributes` in All or ACT:** Remove applied effects from the selected timeline clips in one undo step. Built-in controls such as Motion, Opacity, and Volume keep their settings. Requires Premiere Pro 25.6 or later.
- **`Enter` (or Click):** Apply an effect to selected timeline clips, drill into a project bin to browse/search its contents, or place a project clip or nested sequence at the playhead in the first verified space. Open a sequence before placing media. If Premiere rejects placement on new tracks, add an empty track and retry.
- **`Enter` (or Click) on a `Preset` result:** Open Premiere’s Effects panel and search for that preset by name. Drag it onto a clip to apply it. Spotlight uses the Effects and Select Find Box bindings from Premiere’s Keyboard Shortcuts and refreshes presets after Premiere saves them to your profile.
- **`Backspace` (when search is empty inside a bin, or Click the bin pill):** Step back up one folder level (or exit bin view).
- **`Shift + Enter` (or `Shift` + Click; configurable to `Ctrl` or `Alt`):** Apply an effect and automatically open/focus Premiere Pro's Effect Controls panel, open a sequence in its own Timeline tab (or bring its tab forward if it is already open), or open other project items in the Source Monitor.
- **`Esc`:** Close popup and return focus to Premiere (or exit Settings back to Search if Settings is open)

### Unavailable results
When Spotlight opens, it reads the active sequence and Timeline selection from Premiere. The footer shows a summary such as `Sequence 01 · 2 video, 1 audio`. Results that can't run right now are dimmed and marked with ⊘. Examples include a video effect when only audio clips are selected, **Remove Attributes** with nothing selected, offline media, or a command that requires Premiere Pro 25.6 on an older version. Highlight a dimmed row to see why. Pressing `Enter` on it keeps Spotlight open, shows the reason in the footer and sends nothing to Premiere. Until Premiere answers, or when it can't report something, every row stays enabled and the command's own checks apply.

## Settings & Configuration
- Click the **⚙** icon in the lower-right corner of the Spotlight popup, press **`Ctrl+,`** inside Spotlight (configurable), or right-click the **Premiere Spotlight** tray icon → **Settings** to:
  - Customize the global **Launch Hotkey** (default `Ctrl+Space`).
  - Toggle **Always open Effect Controls on Enter** (swaps primary and alternate actions for effects).
  - Choose the **Alternate Action Modifier** (`Shift`, `Ctrl`, or `Alt`, default `Shift`) used with `Enter` / Click.
  - Customize the **Open Settings Shortcut** (default `Ctrl+,`) and optional **Direct Catalog Shortcuts** for **All**, **Effects**, **Project**, and **Actions** (all empty by default).
  - Customize the **Preview Audio Shortcut** (default `Ctrl+P`) and enable **Auto-preview audio** to play a clip after its highlight settles. Auto-preview is off by default.
- Settings persist in `%APPDATA%\PremiereSpotlight\config.json` and take effect immediately without restarting.
- Under **Appearance**, choose **Light**, **Dark**, **Midnight Indigo** (the default), or **Premiere**.
- Under **Hotkeys**, record or clear a shortcut for any ACT command. Action hotkeys start unassigned and work from Premiere or Spotlight search. Spotlight saves them under `action_hotkeys` in the same config file. Use **Spotlight Controls** for launch, catalog, and preview shortcuts.
