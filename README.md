# Premiere Spotlight (Rust + Slint + Adobe UXP)

An ultra-lightweight, zero-WebView Spotlight launcher for Adobe Premiere Pro effects and project media.

## Architecture

1. **`helper-app/` (Rust + Slint Software Renderer)**
   - **Single process, zero WebView2:** Compiles `.slint` declarative UI directly to native Rust machine code rendered via Slint's CPU software renderer (`renderer-software`).
   - **Global Hotkey (`Ctrl + Space`):** Toggles the frameless centered floating window (`620×407`).
   - **Fuzzy Search (`nucleo-matcher`):** Typo-tolerant fuzzy search across Video Effects, Audio Effects, Transitions, and custom presets, boosted by usage frequency.
   - **Localhost WebSocket Bridge (`ws://127.0.0.1:3847`):** Event-driven, 0% idle CPU bridge to Premiere Pro.

2. **`uxp-plugin/` (Adobe Premiere Pro UXP Plugin)**
   - Connects to `ws://127.0.0.1:3847` when Premiere Pro starts.
   - Automatically scans all installed Video Effects (`VideoFilterFactory`), Audio Effects (`AudioFilterFactory`), and Transitions (`TransitionFactory`) and syncs them to the Rust helper.
   - Applies the chosen effect to all selected timeline clips inside a single undoable Premiere Pro transaction.

## Quick Start

### 1. Run the Rust + Slint Floating Spotlight App
```powershell
cd C:\Users\omiso\Desktop\premiere-spotlight\helper-app
cargo run --release
```

### 2. Load the Premiere Pro UXP Plugin
1. Open the **Adobe UXP Developer Tool** (or pack `uxp-plugin/` as a `.ccx` ZIP archive).
2. Click **Add Plugin** and select `C:\Users\omiso\Desktop\premiere-spotlight\uxp-plugin\manifest.json`.
3. With Adobe Premiere Pro open, click **Load** on **Premiere Spotlight Bridge**.

## Keyboard Controls (Inside Spotlight Popup)
- **Type:** Fuzzy search effects, transitions, custom presets, and items in the active project. Selected presets and project items show their folder path.
- **`↑` / `↓` Arrows:** Navigate results
- **`Tab` (or click the search-bar badge):** Switch catalogs (`All` → `Effects` → `Project`). Spotlight opens in `All` and shows recently applied effects and inserted items from the active project. Select a recent item to apply or insert it again.
- **`Enter` (or Click):** Apply an effect to selected timeline clips, drill into a project bin to browse/search its contents, or place a project clip or nested sequence at the playhead in the first verified space. Open a sequence before placing media. If Premiere rejects placement on new tracks, add an empty track and retry.
- **`Enter` (or Click) on a `Preset` result:** Open Premiere’s Effects panel and search for that preset by name. Drag it onto a clip to apply it. Spotlight uses the Effects and Select Find Box bindings from Premiere’s Keyboard Shortcuts and refreshes presets after Premiere saves them to your profile.
- **`Backspace` (when search is empty inside a bin, or Click the bin pill):** Step back up one folder level (or exit bin view).
- **`Shift + Enter` (or `Shift` + Click; configurable to `Ctrl` or `Alt`):** Apply an effect and automatically open/focus Premiere Pro's Effect Controls panel, or open a project item in the Source Monitor.
- **`Esc`:** Close popup and return focus to Premiere (or exit Settings back to Search if Settings is open)

## Settings & Configuration
- Click the **⚙** icon in the lower-right corner of the Spotlight popup, press **`Ctrl+,`** inside Spotlight (configurable), or right-click the **Premiere Spotlight** tray icon → **Settings…** to:
  - Customize the global **Launch Hotkey** (default `Ctrl+Space`).
  - Toggle **Always open Effect Controls on Enter** (swaps primary and alternate actions for effects; also toggleable directly from the System Tray menu).
  - Choose the **Alternate Action Modifier** (`Shift`, `Ctrl`, or `Alt`, default `Shift`) used with `Enter` / Click.
  - Customize the **Open Settings Shortcut** (default `Ctrl+,`) and optional **Direct Catalog Shortcuts** for **All**, **Effects**, and **Project** (all empty by default).
- Settings persist in `%APPDATA%\PremiereSpotlight\config.json` and take effect immediately without restarting.
