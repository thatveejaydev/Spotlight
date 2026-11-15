# Premiere Spotlight (Rust + Slint + Adobe UXP)

An ultra-lightweight, zero-WebView Spotlight launcher for Adobe Premiere Pro effects and project media.

## Architecture

1. **`helper-app/` (Rust + Slint Software Renderer)**
   - **Single process, zero WebView2:** Compiles `.slint` declarative UI directly to native Rust machine code rendered via Slint's CPU software renderer (`renderer-software`).
   - **Global Hotkey (`Ctrl + Space`):** Toggles the frameless centered floating window (`620×407`).
   - **Fuzzy Search (`nucleo-matcher`):** Sub-millisecond typo-tolerant fuzzy search across Video Effects, Audio Effects, and Transitions, boosted by usage frequency.
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
- **Type:** Fuzzy search effects, transitions, and items in the active project. The selected project result shows its bin path.
- **`↑` / `↓` Arrows:** Navigate results
- **`Tab` (or click the search-bar badge):** Switch catalogs (`All` → `Effects` → `Project`). Spotlight opens in `All` and shows recently applied effects and inserted items from the active project. Select a recent item to apply or insert it again.
- **`Enter` (or Click):** Apply an effect to selected timeline clips, or place a project clip or nested sequence at the playhead in the first verified space. Open a sequence before placing media. If Premiere rejects placement on new tracks, add an empty track and retry.
- **`Shift + Enter` (or `Shift` + Click):** Apply an effect and automatically open/focus Premiere Pro's Effect Controls panel, or open a project item in the Source Monitor.
- **`Esc`:** Close popup and return focus to Premiere
