#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod catalog;
mod installer;
mod win_focus;

use bridge::{BridgeClients, BridgeEvent};
use catalog::{CatalogStore, EffectEntry};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use slint::{Color, ComponentHandle, ModelRc, PhysicalPosition, SharedString, Timer, TimerMode, VecModel};
use std::{cell::RefCell, rc::Rc, time::Duration};

slint::include_modules!();

const VISIBLE_ROWS: usize = 8;
const WINDOW_WIDTH: i32 = 620;
const WINDOW_HEIGHT: i32 = 446;

struct AppState {
    catalog: CatalogStore,
    matched_items: Vec<EffectEntry>,
    selected_abs_index: usize,
    scroll_offset: usize,
    is_visible: bool,
    previous_hwnd: isize,
}

fn badge_colors(effect_type: &str) -> (Color, Color) {
    match effect_type {
        "videoEffect" => (Color::from_argb_u8(45, 99, 102, 241), Color::from_rgb_u8(165, 180, 252)),
        "audioEffect" => (Color::from_argb_u8(45, 16, 185, 129), Color::from_rgb_u8(110, 231, 183)),
        "videoTransition" => (Color::from_argb_u8(45, 245, 158, 11), Color::from_rgb_u8(252, 211, 77)),
        "audioTransition" => (Color::from_argb_u8(45, 236, 72, 153), Color::from_rgb_u8(249, 168, 212)),
        _ => (Color::from_argb_u8(45, 148, 163, 184), Color::from_rgb_u8(203, 213, 225)),
    }
}

fn refresh_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    let query = ui.get_search_text().to_string();
    let filter = ui.get_active_filter().to_string();

    state.matched_items = state.catalog.search(&query, &filter);
    let total = state.matched_items.len();
    ui.set_total_matches(total as i32);

    if total == 0 {
        state.selected_abs_index = 0;
        state.scroll_offset = 0;
        ui.set_selected_index(0);
        ui.set_results(ModelRc::new(VecModel::from(Vec::<EffectItemUI>::new())));
        return;
    }

    if state.selected_abs_index >= total {
        state.selected_abs_index = total - 1;
    }

    sync_viewport_slice(ui, state);
}

fn sync_viewport_slice(ui: &SpotlightWindow, state: &mut AppState) {
    let total = state.matched_items.len();
    if total == 0 {
        ui.set_results(ModelRc::new(VecModel::from(Vec::<EffectItemUI>::new())));
        ui.set_selected_index(0);
        return;
    }

    if state.selected_abs_index < state.scroll_offset {
        state.scroll_offset = state.selected_abs_index;
    } else if state.selected_abs_index >= state.scroll_offset + VISIBLE_ROWS {
        state.scroll_offset = state.selected_abs_index + 1 - VISIBLE_ROWS;
    }

    let end = (state.scroll_offset + VISIBLE_ROWS).min(total);
    let slice: Vec<EffectItemUI> = state.matched_items[state.scroll_offset..end]
        .iter()
        .map(|entry| {
            let (badge_bg, badge_fg) = badge_colors(&entry.effect_type);
            EffectItemUI {
                name: SharedString::from(entry.name.as_str()),
                category: SharedString::from(entry.category_short()),
                folder: SharedString::from(entry.folder.as_str()),
                match_name: SharedString::from(entry.match_name.as_str()),
                effect_type: SharedString::from(entry.effect_type.as_str()),
                badge_bg,
                badge_fg,
            }
        })
        .collect();

    let rel_idx = (state.selected_abs_index - state.scroll_offset) as i32;
    ui.set_results(ModelRc::new(VecModel::from(slice)));
    ui.set_selected_index(rel_idx);
}

fn extract_window_hwnd(ui: &SpotlightWindow) -> isize {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let win_handle = ui.window().window_handle();
        if let Ok(handle) = win_handle.window_handle() {
            if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                return win32.hwnd.get();
            }
        }
    }
    0
}

fn center_and_show(ui: &SpotlightWindow, state: &mut AppState) {
    let (cx, cy) = win_focus::primary_screen_center(WINDOW_WIDTH, WINDOW_HEIGHT);
    ui.window()
        .set_position(PhysicalPosition::new(cx, cy));

    ui.set_search_text(SharedString::from(""));
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);

    let _ = ui.show();
    state.is_visible = true;

    let our_hwnd = extract_window_hwnd(ui);
    win_focus::force_foreground(our_hwnd);
    ui.invoke_focus_search();
}

fn hide_and_restore_focus(ui: &SpotlightWindow, state: &mut AppState) {
    let _ = ui.hide();
    state.is_visible = false;
    if state.previous_hwnd != 0 {
        win_focus::force_foreground(state.previous_hwnd);
    }
}

fn apply_entry_at(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return;
    };
    state.catalog.record_usage(&chosen.match_name);
    let sent = bridge.send_apply(&chosen);
    if sent {
        ui.set_status_message(SharedString::from(format!("Applied: {}", chosen.name)));
    } else {
        ui.set_status_message(SharedString::from(format!(
            "Selected: {} (Open Premiere UXP panel to apply)",
            chosen.name
        )));
    }
    hide_and_restore_focus(ui, state);
}

fn main() -> Result<(), slint::PlatformError> {
    // 1. Auto-install / enable the UXP bridge plugin in Premiere Pro & register Windows Startup (--watch)
    installer::ensure_installed();

    let watch_arg = std::env::args().any(|a| a == "--watch" || a == "--background");
    let launched_from_premiere = win_focus::is_premiere_hwnd(win_focus::get_foreground_hwnd());
    let start_hidden = watch_arg || launched_from_premiere;

    // 2. Single-instance check: if an instance is already listening on 127.0.0.1:3847, wake it (unless --watch) and exit
    if bridge::check_or_wake_existing_instance(!start_hidden) {
        return Ok(());
    }

    let ui = SpotlightWindow::new()?;

    let state = Rc::new(RefCell::new(AppState {
        catalog: CatalogStore::new_with_defaults(),
        matched_items: Vec::new(),
        selected_abs_index: 0,
        scroll_offset: 0,
        is_visible: false,
        previous_hwnd: 0,
    }));

    // Initial search populate
    {
        let mut st = state.borrow_mut();
        refresh_results_ui(&ui, &mut st);
    }

    // Start localhost WebSocket bridge on 127.0.0.1:3847
    let bridge = BridgeClients::new();
    {
        let ui_weak = ui.as_weak();
        bridge.start_server(move |event| {
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                match event {
                    BridgeEvent::ClientConnected => {
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(
                            "Connected to Premiere Pro · Ctrl+Space",
                        ));
                    }
                    BridgeEvent::ClientDisconnected => {
                        ui.set_bridge_connected(false);
                        ui.set_status_message(SharedString::from(
                            "Premiere disconnected · Standalone catalog active",
                        ));
                    }
                    BridgeEvent::CatalogReceived(items) => {
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(format!(
                            "Premiere synced ({} effects) · Ctrl+Space",
                            items.len()
                        )));
                        CATALOG_QUEUE.with(|q| q.borrow_mut().push(items));
                    }
                    BridgeEvent::ApplyResult { ok, message } => {
                        if !message.is_empty() {
                            let prefix = if ok { "Premiere:" } else { "Error:" };
                            ui.set_status_message(SharedString::from(format!(
                                "{} {}",
                                prefix, message
                            )));
                        }
                    }
                    BridgeEvent::ShowWindowRequested => {
                        SHOW_WINDOW_QUEUE.with(|flag| *flag.borrow_mut() = true);
                    }
                }
            });
        });
    }

    // Search text edited callback
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_search_edited(move |_text| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                st.selected_abs_index = 0;
                st.scroll_offset = 0;
                refresh_results_ui(&ui, &mut st);
            }
        });
    }

    // Filter pill clicked callback
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_filter_changed(move |new_filter| {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_active_filter(new_filter);
                let mut st = state_ref.borrow_mut();
                st.selected_abs_index = 0;
                st.scroll_offset = 0;
                refresh_results_ui(&ui, &mut st);
            }
        });
    }

    // Tab key cycles filter
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_cycle_filter(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let next = match ui.get_active_filter().as_str() {
                    "all" => "videoEffect",
                    "videoEffect" => "audioEffect",
                    "audioEffect" => "transition",
                    _ => "all",
                };
                ui.set_active_filter(SharedString::from(next));
                let mut st = state_ref.borrow_mut();
                st.selected_abs_index = 0;
                st.scroll_offset = 0;
                refresh_results_ui(&ui, &mut st);
            }
        });
    }

    // Up / Down arrow selection movement
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_move_selection(move |delta| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let total = st.matched_items.len();
                if total == 0 {
                    return;
                }
                let cur = st.selected_abs_index as isize;
                let next = (cur + delta as isize).rem_euclid(total as isize) as usize;
                st.selected_abs_index = next;
                sync_viewport_slice(&ui, &mut st);
            }
        });
    }

    // Enter key applies selected effect
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        ui.on_activate_selected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.selected_abs_index;
                apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref);
            }
        });
    }

    // Mouse click on a row applies that row
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        ui.on_activate_index(move |rel_idx| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.scroll_offset + (rel_idx.max(0) as usize);
                st.selected_abs_index = abs_idx;
                apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref);
            }
        });
    }

    // Escape key closes popup
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_close_requested(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                hide_and_restore_focus(&ui, &mut st);
            }
        });
    }

    // Toggle hotkey scope (Everywhere vs Premiere Only)
    {
        let ui_weak = ui.as_weak();
        ui.on_toggle_scope(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let next = !ui.get_premiere_only_scope();
                ui.set_premiere_only_scope(next);
            }
        });
    }

    // Register global hotkey: Ctrl + Space
    let hotkey_manager = GlobalHotKeyManager::new().expect("Failed to initialize GlobalHotKeyManager");
    let ctrl_space = HotKey::new(Some(Modifiers::CONTROL), Code::Space);
    if let Err(err) = hotkey_manager.register(ctrl_space) {
        eprintln!("[hotkey] Could not register Ctrl+Space: {}", err);
    } else {
        println!("[hotkey] Registered global shortcut: Ctrl + Space");
    }

    // Show window immediately on direct launch, or stay hidden in background when started via --watch or Premiere
    if !start_hidden {
        let mut st = state.borrow_mut();
        st.previous_hwnd = win_focus::get_foreground_hwnd();
        center_and_show(&ui, &mut st);
    }

    // Poll GlobalHotKeyEvent and dynamic catalog updates on a lightweight 20ms timer
    let timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };

            // Drain any newly synced Premiere catalogs
            CATALOG_QUEUE.with(|q| {
                let mut queue = q.borrow_mut();
                if let Some(latest) = queue.pop() {
                    queue.clear();
                    let mut st = state_ref.borrow_mut();
                    st.catalog.set_catalog(latest);
                    refresh_results_ui(&ui, &mut st);
                }
            });

            // Check if a second premiere-spotlight.exe invocation asked to show the window
            let wake_requested = SHOW_WINDOW_QUEUE.with(|flag| {
                let mut f = flag.borrow_mut();
                let val = *f;
                *f = false;
                val
            });
            if wake_requested {
                let mut st = state_ref.borrow_mut();
                if !st.is_visible {
                    st.previous_hwnd = win_focus::get_foreground_hwnd();
                }
                center_and_show(&ui, &mut st);
            }

            // Check global hotkey events
            while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                if event.id == ctrl_space.id() && event.state == HotKeyState::Pressed {
                    let mut st = state_ref.borrow_mut();
                    if st.is_visible {
                        hide_and_restore_focus(&ui, &mut st);
                    } else {
                        let fg_hwnd = win_focus::get_foreground_hwnd();
                        if ui.get_premiere_only_scope() && !win_focus::is_premiere_hwnd(fg_hwnd) {
                            continue;
                        }
                        st.previous_hwnd = fg_hwnd;
                        center_and_show(&ui, &mut st);
                    }
                }
            }
        });
    }

    // Keep event loop alive even when popup window is hidden
    slint::run_event_loop_until_quit()?;
    Ok(())
}

thread_local! {
    static CATALOG_QUEUE: RefCell<Vec<Vec<EffectEntry>>> = const { RefCell::new(Vec::new()) };
    static SHOW_WINDOW_QUEUE: RefCell<bool> = const { RefCell::new(false) };
}
