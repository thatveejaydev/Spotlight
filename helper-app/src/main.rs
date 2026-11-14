#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod catalog;
mod installer;
mod tray;
mod win_focus;

use bridge::{BridgeClients, BridgeEvent};
use catalog::{CatalogStore, EffectEntry};
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use slint::{
    Color, ComponentHandle, ModelRc, PhysicalPosition, SharedString, Timer, TimerMode, VecModel,
};
use std::{cell::RefCell, rc::Rc, time::Duration};
use tray::{SystemTray, TrayAction};

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
        "videoEffect" => (
            Color::from_argb_u8(45, 99, 102, 241),
            Color::from_rgb_u8(165, 180, 252),
        ),
        "audioEffect" => (
            Color::from_argb_u8(45, 16, 185, 129),
            Color::from_rgb_u8(110, 231, 183),
        ),
        "videoTransition" => (
            Color::from_argb_u8(45, 245, 158, 11),
            Color::from_rgb_u8(252, 211, 77),
        ),
        "audioTransition" => (
            Color::from_argb_u8(45, 236, 72, 153),
            Color::from_rgb_u8(249, 168, 212),
        ),
        _ => (
            Color::from_argb_u8(45, 148, 163, 184),
            Color::from_rgb_u8(203, 213, 225),
        ),
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
    ui.window().set_position(PhysicalPosition::new(cx, cy));

    ui.set_search_text(SharedString::from(""));
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);

    let our_hwnd_before = extract_window_hwnd(ui);
    win_focus::hide_from_taskbar(our_hwnd_before);

    let _ = ui.show();
    state.is_visible = true;

    let our_hwnd = extract_window_hwnd(ui);
    win_focus::hide_from_taskbar(our_hwnd);
    win_focus::force_foreground(our_hwnd);
    ui.invoke_focus_search();
}

fn hide_and_restore_focus(ui: &SpotlightWindow, state: &mut AppState) {
    let _ = ui.hide();
    state.is_visible = false;
    if state.previous_hwnd != 0 && win_focus::is_premiere_active_and_unminimized(state.previous_hwnd)
    {
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
            "Selected: {} (Waiting for Premiere bridge)",
            chosen.name
        )));
    }
    hide_and_restore_focus(ui, state);
}

fn main() -> Result<(), slint::PlatformError> {
    // 1. Auto-install / enable the UXP bridge plugin in Premiere Pro & register Windows Startup (--watch)
    installer::ensure_installed();

    // 2. Single-instance check: if Premiere Spotlight is already running in the System Tray, wake it (only opens if Premiere is active) and exit
    if bridge::check_or_wake_existing_instance(true) {
        return Ok(());
    }

    let ui = SpotlightWindow::new()?;
    win_focus::hide_from_taskbar(extract_window_hwnd(&ui));

    // Create native Windows System Tray icon (no taskbar icon)
    let tray = Rc::new(RefCell::new(SystemTray::new()));

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
                        BRIDGE_STATUS_QUEUE.with(|q| *q.borrow_mut() = Some(true));
                    }
                    BridgeEvent::ClientDisconnected => {
                        ui.set_bridge_connected(false);
                        ui.set_status_message(SharedString::from(
                            "Premiere disconnected · Standalone catalog active",
                        ));
                        BRIDGE_STATUS_QUEUE.with(|q| *q.borrow_mut() = Some(false));
                    }
                    BridgeEvent::CatalogReceived(items) => {
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(format!(
                            "Premiere synced ({} effects) · Ctrl+Space",
                            items.len()
                        )));
                        CATALOG_QUEUE.with(|q| q.borrow_mut().push(items));
                        BRIDGE_STATUS_QUEUE.with(|q| *q.borrow_mut() = Some(true));
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

    // Prepare global hotkey manager (Ctrl + Space is registered dynamically ONLY when Premiere Pro is focused & unminimized)
    let hotkey_manager =
        GlobalHotKeyManager::new().expect("Failed to initialize GlobalHotKeyManager");
    let ctrl_space = HotKey::new(Some(Modifiers::CONTROL), Code::Space);
    let mut hotkey_registered = false;

    // Poll focus state, global hotkey events, and system tray actions on a 20ms timer
    let timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let tray_ref = tray.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };

            // 1. Update tray tooltip if bridge status changed
            if let Some(connected) = BRIDGE_STATUS_QUEUE.with(|q| q.borrow_mut().take()) {
                if let Some(ref t) = *tray_ref.borrow() {
                    t.set_connected(connected);
                }
            }

            // 2. Drain any newly synced Premiere catalogs
            CATALOG_QUEUE.with(|q| {
                let mut queue = q.borrow_mut();
                if let Some(latest) = queue.pop() {
                    queue.clear();
                    let mut st = state_ref.borrow_mut();
                    st.catalog.set_catalog(latest);
                    refresh_results_ui(&ui, &mut st);
                }
            });

            // 3. Handle System Tray menu / click actions
            while let Some(action) = tray::poll_tray_action() {
                match action {
                    TrayAction::OpenSpotlight => {
                        let (_, unminimized_premiere_hwnd) =
                            win_focus::query_premiere_window_state();
                        if unminimized_premiere_hwnd != 0 {
                            let mut st = state_ref.borrow_mut();
                            st.previous_hwnd = unminimized_premiere_hwnd;
                            center_and_show(&ui, &mut st);
                        }
                    }
                    TrayAction::Quit => {
                        if let Some(ref mut t) = *tray_ref.borrow_mut() {
                            t.remove();
                        }
                        std::process::exit(0);
                    }
                }
            }

            // 4. Handle wake request (only if Premiere is active & unminimized)
            let wake_requested = SHOW_WINDOW_QUEUE.with(|flag| {
                let mut f = flag.borrow_mut();
                let val = *f;
                *f = false;
                val
            });
            if wake_requested {
                let fg_hwnd = win_focus::get_foreground_hwnd();
                if win_focus::is_premiere_active_and_unminimized(fg_hwnd) {
                    let mut st = state_ref.borrow_mut();
                    st.previous_hwnd = fg_hwnd;
                    center_and_show(&ui, &mut st);
                }
            }

            // 5. Check whether Premiere Pro (unminimized) or our visible Spotlight popup is currently in the foreground
            let fg_hwnd = win_focus::get_foreground_hwnd();
            let our_hwnd = extract_window_hwnd(&ui);
            let is_our_window = our_hwnd != 0 && fg_hwnd == our_hwnd;
            let is_premiere_fg = win_focus::is_premiere_active_and_unminimized(fg_hwnd);

            let should_enable_hotkey = {
                let st = state_ref.borrow();
                is_premiere_fg || (st.is_visible && is_our_window)
            };

            if should_enable_hotkey {
                if !hotkey_registered {
                    if hotkey_manager.register(ctrl_space).is_ok() {
                        hotkey_registered = true;
                    }
                }
            } else {
                if hotkey_registered {
                    let _ = hotkey_manager.unregister(ctrl_space);
                    hotkey_registered = false;
                }
                // Discard any queued hotkey events while Premiere is not focused
                while GlobalHotKeyEvent::receiver().try_recv().is_ok() {}

                // If Spotlight popup was open and the user clicked another app or minimized Premiere, auto-hide immediately
                let mut st = state_ref.borrow_mut();
                if st.is_visible {
                    let _ = ui.hide();
                    st.is_visible = false;
                }
            }

            // 6. Process Ctrl + Space hotkey events only when enabled
            if should_enable_hotkey {
                while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
                    if event.id == ctrl_space.id() && event.state == HotKeyState::Pressed {
                        let mut st = state_ref.borrow_mut();
                        if st.is_visible {
                            hide_and_restore_focus(&ui, &mut st);
                        } else if is_premiere_fg {
                            st.previous_hwnd = fg_hwnd;
                            center_and_show(&ui, &mut st);
                        }
                    }
                }
            }
        });
    }

    // Keep event loop alive while helper is active in the System Tray
    slint::run_event_loop_until_quit()?;
    Ok(())
}

thread_local! {
    static CATALOG_QUEUE: RefCell<Vec<Vec<EffectEntry>>> = const { RefCell::new(Vec::new()) };
    static SHOW_WINDOW_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static BRIDGE_STATUS_QUEUE: RefCell<Option<bool>> = const { RefCell::new(None) };
}
