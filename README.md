# Premiere Spotlight (Rust + Slint + Adobe UXP)

An ultra-lightweight, zero-WebView Spotlight effect launcher for Adobe Premiere Pro.

## Architecture

1. **`helper-app/` (Rust + Slint Software Renderer)**
   - **Single process, zero WebView2:** Compiles `.slint` declarative UI directly to native Rust machine code rendered via Slint's CPU software renderer (`renderer-software`).
   - **Global Hotkey (`Ctrl + Space`):** Toggles the frameless centered floating window (`620×430`) and automatically restores keyboard focus to Adobe Premiere Pro when you press `Enter` or `Esc`.
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
- **Type:** Instant fuzzy search across effects & transitions
- **`↑` / `↓` Arrows:** Navigate results
- **`Tab`:** Cycle category filter (`All` → `Video FX` → `Audio FX` → `Transitions`)
- **`Enter` (or Click):** Apply selected effect to selected timeline clip(s) and return focus to Premiere
- **`Esc`:** Close popup and return focus to Premiere
