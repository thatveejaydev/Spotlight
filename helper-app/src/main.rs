#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod catalog;
mod error_toast;
mod installer;
mod tray;
mod win_focus;

use bridge::{ApplyResult, BridgeClients, BridgeEvent, MediaCatalog, PlacementResult};
use catalog::{CatalogStore, EffectEntry, ProjectItemKind, SearchResult};
use error_toast::ErrorToast;
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use slint::{
    Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tray::{SystemTray, TrayAction};

slint::include_modules!();

const VISIBLE_ROWS: usize = 8;
const MAX_RECENT_ITEMS: usize = 20;
const ACTION_TIMEOUT: Duration = Duration::from_secs(12);
const ACTIVATION_GRACE: Duration = Duration::from_millis(250);

struct AppState {
    catalog: CatalogStore,
    matched_items: Vec<SearchResult>,
    recent_items: Vec<RecentItem>,
    pending_effects: HashMap<u64, PendingEffect>,
    media_catalog: Option<MediaCatalog>,
    pending_placement: Option<PendingPlacement>,
    selected_abs_index: usize,
    scroll_offset: usize,
    is_visible: bool,
    shown_at: Option<Instant>,
    saw_foreground: bool,
    previous_hwnd: isize,
}

struct PendingEffect {
    entry: EffectEntry,
    recipients: Vec<usize>,
    deadline: Instant,
}

struct PendingPlacement {
    peer_id: usize,
    request_id: u64,
    deadline: Instant,
    recent: RecentItem,
}

struct RecentItem {
    item: SearchResult,
    project_id: Option<String>,
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
        "media" => (
            Color::from_argb_u8(52, 56, 189, 248),
            Color::from_rgb_u8(125, 211, 252),
        ),
        "sequence" => (
            Color::from_argb_u8(52, 168, 85, 247),
            Color::from_rgb_u8(216, 180, 254),
        ),
        "bin" => (
            Color::from_argb_u8(52, 245, 158, 11),
            Color::from_rgb_u8(253, 224, 131),
        ),
        _ => (
            Color::from_argb_u8(45, 148, 163, 184),
            Color::from_rgb_u8(203, 213, 225),
        ),
    }
}

fn remember_item(state: &mut AppState, recent: RecentItem) {
    state.recent_items.retain(|entry| {
        entry.project_id != recent.project_id || !entry.item.same_item(&recent.item)
    });
    state.recent_items.insert(0, recent);
    state.recent_items.truncate(MAX_RECENT_ITEMS);
}

fn recent_results(state: &AppState) -> Vec<SearchResult> {
    state
        .recent_items
        .iter()
        .filter_map(|recent| match &recent.item {
            SearchResult::Effect(_) => Some(recent.item.clone()),
            SearchResult::ProjectItem(item) => {
                let catalog = state.media_catalog.as_ref()?;
                if recent.project_id != catalog.project_id {
                    return None;
                }
                catalog
                    .items
                    .iter()
                    .find(|entry| entry.item_id == item.item_id && entry.placement_error().is_none())
                    .cloned()
                    .map(SearchResult::ProjectItem)
            }
        })
        .collect()
}

fn complete_effect(state: &mut AppState, result: &ApplyResult) -> bool {
    let request_id = match result.request_id {
        Some(id) => id,
        // Already loaded plugins can still acknowledge an apply without a request ID.
        None if result.ok => {
            let mut matches = state.pending_effects.iter().filter(|(_, pending)| {
                pending.recipients.contains(&result.peer_id)
                    && Instant::now() < pending.deadline
                    && result.message
                        .strip_prefix("Applied \"")
                        .and_then(|message| message.strip_prefix(&pending.entry.name))
                        .and_then(|message| message.strip_prefix('"'))
                        .is_some_and(|suffix| suffix.starts_with(" to ") || suffix.starts_with(" ("))
            });
            match (matches.next(), matches.next()) {
                (Some((&id, _)), None) => id,
                _ => return false,
            }
        }
        None => return false,
    };
    let Some(pending) = state.pending_effects.get_mut(&request_id) else {
        return false;
    };
    let Some(index) = pending.recipients.iter().position(|peer| *peer == result.peer_id) else {
        return false;
    };
    if Instant::now() >= pending.deadline {
        return false;
    }
    if !result.ok {
        pending.recipients.swap_remove(index);
        if pending.recipients.is_empty() {
            state.pending_effects.remove(&request_id);
        }
        return false;
    }
    let entry = state.pending_effects.remove(&request_id).unwrap().entry;
    state.catalog.record_usage(&entry.match_name);
    remember_item(
        state,
        RecentItem {
            item: SearchResult::Effect(entry),
            project_id: None,
        },
    );
    true
}

fn refresh_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    let query = ui.get_search_text();
    let catalog = ui.get_active_catalog();
    let show_history = catalog.as_str() == "all" && query.trim().is_empty();
    ui.set_show_recent_items(show_history);
    state.matched_items = if show_history {
        recent_results(state)
    } else {
        state.catalog.search(query.as_str(), catalog.as_str())
    };
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

fn refresh_catalog_ui(ui: &SpotlightWindow, state: &mut AppState, preserve_media: bool) {
    let selected = state.matched_items.get(state.selected_abs_index).cloned();
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
    if let Some(selected) =
        selected.filter(|item| preserve_media || !matches!(item, SearchResult::ProjectItem(_)))
    {
        if let Some(index) = state
            .matched_items
            .iter()
            .position(|item| item.same_item(&selected))
        {
            state.selected_abs_index = index;
            sync_viewport_slice(ui, state);
        }
    }
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
            let (name, category, folder, colors) = match entry {
                SearchResult::Effect(entry) => (
                    entry.name.as_str(),
                    entry.category_short(),
                    "",
                    badge_colors(&entry.effect_type),
                ),
                SearchResult::ProjectItem(entry) => (
                    entry.name.as_str(),
                    entry.kind.label(),
                    entry.bin_path.as_str(),
                    badge_colors(match entry.kind {
                        ProjectItemKind::Media => "media",
                        ProjectItemKind::Sequence | ProjectItemKind::ActiveSequence => "sequence",
                        ProjectItemKind::Bin => "bin",
                        ProjectItemKind::Other => "other",
                    }),
                ),
            };
            EffectItemUI {
                name: name.into(),
                category: category.into(),
                folder: folder.into(),
                badge_bg: colors.0,
                badge_fg: colors.1,
            }
        })
        .collect();

    let rel_idx = (state.selected_abs_index - state.scroll_offset) as i32;
    ui.set_results(ModelRc::new(VecModel::from(slice)));
    ui.set_selected_index(rel_idx);
}

fn extract_window_hwnd(ui: &SpotlightWindow) -> isize {
    win_focus::window_hwnd(ui.window())
}

fn center_and_show(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients) {
    ui.set_active_catalog("all".into());
    ui.set_search_text(SharedString::from(""));
    state.media_catalog = None;
    state.catalog.set_project_items(Vec::new());
    bridge.request_project_items();
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);

    show_palette(ui, state);
}

fn show_palette(ui: &SpotlightWindow, state: &mut AppState) {
    ui.window().set_minimized(false);
    let our_hwnd_before = extract_window_hwnd(ui);
    win_focus::center_palette(our_hwnd_before, state.previous_hwnd);

    if ui.show().is_err() {
        return;
    }
    state.is_visible = true;
    state.shown_at = Some(Instant::now());
    state.saw_foreground = false;

    let our_hwnd = extract_window_hwnd(ui);
    if our_hwnd != our_hwnd_before {
        win_focus::center_palette(our_hwnd, state.previous_hwnd);
    }
    win_focus::ensure_palette_topmost(our_hwnd);
    win_focus::force_foreground(our_hwnd);
    ui.invoke_focus_search();
}

fn restore_premiere_focus(hwnd: isize) {
    if hwnd != 0
        && win_focus::get_foreground_hwnd() != hwnd
        && win_focus::is_premiere_active_and_unminimized(hwnd)
    {
        win_focus::restore_foreground(hwnd);
    }
}

fn hide_palette(ui: &SpotlightWindow, state: &mut AppState) {
    let _ = ui.hide();
    state.is_visible = false;
    state.shown_at = None;
    state.saw_foreground = false;
}

fn hide_and_restore_focus(ui: &SpotlightWindow, state: &mut AppState) {
    restore_premiere_focus(state.previous_hwnd);
    hide_palette(ui, state);
}

fn update_placement(
    ui: &SpotlightWindow,
    state: &mut AppState,
    results: Vec<PlacementResult>,
) -> Option<SharedString> {
    let pending = state.pending_placement.as_ref()?;
    if let Some(result) = results.into_iter().find(|result| {
        result.peer_id == pending.peer_id && result.request_id == pending.request_id
    }) {
        let pending = state.pending_placement.take().unwrap();
        if result.ok {
            if Instant::now() < pending.deadline {
                remember_item(state, pending.recent);
                refresh_catalog_ui(ui, state, true);
            }
            ui.set_status_message(result.message.into());
        } else {
            return Some(if result.message.is_empty() {
                "Premiere could not place the media.".into()
            } else {
                result.message.into()
            });
        }
    } else if Instant::now() >= pending.deadline {
        state.pending_placement = None;
        return Some("Placement outcome unknown. Check the timeline before retrying.".into());
    }
    None
}

fn toggle_palette_from_hotkey(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients) {
    let fg_hwnd = win_focus::get_foreground_hwnd();
    let is_premiere_fg = win_focus::is_premiere_active_and_unminimized(fg_hwnd);
    let our_hwnd = extract_window_hwnd(ui);
    let palette_actively_open = if our_hwnd != 0 {
        win_focus::is_palette_open_and_active(our_hwnd)
    } else {
        state.is_visible
    };
    toggle_palette_from_hotkey_with_focus(
        ui,
        state,
        bridge,
        fg_hwnd,
        is_premiere_fg,
        palette_actively_open,
    );
}

#[cfg(test)]
fn toggle_palette_from_hotkey_for_fg(
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    fg_hwnd: isize,
    is_premiere_fg: bool,
) {
    toggle_palette_from_hotkey_with_focus(
        ui,
        state,
        bridge,
        fg_hwnd,
        is_premiere_fg,
        state.is_visible,
    );
}

fn toggle_palette_from_hotkey_with_focus(
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    fg_hwnd: isize,
    is_premiere_fg: bool,
    palette_actively_open: bool,
) {
    if state.is_visible && palette_actively_open {
        hide_and_restore_focus(ui, state);
        return;
    }
    if is_premiere_fg {
        state.previous_hwnd = fg_hwnd;
        center_and_show(ui, state, bridge);
        return;
    }
    if state.is_visible {
        hide_and_restore_focus(ui, state);
    }
}

enum FocusOutcome {
    KeepVisible,
    RetryForeground,
    HideLostFocus,
}

fn evaluate_palette_focus(state: &mut AppState, is_our_window: bool, now: Instant) -> FocusOutcome {
    if !state.is_visible {
        return FocusOutcome::KeepVisible;
    }
    if is_our_window {
        state.saw_foreground = true;
        return FocusOutcome::KeepVisible;
    }
    let grace_elapsed = state
        .shown_at
        .is_some_and(|shown| now.saturating_duration_since(shown) >= ACTIVATION_GRACE);
    if state.saw_foreground || grace_elapsed {
        state.is_visible = false;
        state.shown_at = None;
        state.saw_foreground = false;
        FocusOutcome::HideLostFocus
    } else {
        FocusOutcome::RetryForeground
    }
}

fn apply_entry_at(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<(), SharedString> {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return Ok(());
    };
    if state.pending_placement.is_some() {
        ui.set_status_message("A media placement is already in progress.".into());
        return Ok(());
    }
    match chosen {
        SearchResult::Effect(entry) => {
            let Some((request_id, recipients)) = bridge.send_apply(&entry) else {
                return Err("Premiere effect bridge disconnected.".into());
            };
            ui.set_status_message(format!("Applying {}…", entry.name).into());
            state.pending_effects.insert(
                request_id,
                PendingEffect {
                    entry,
                    recipients,
                    deadline: Instant::now() + ACTION_TIMEOUT,
                },
            );
        }
        SearchResult::ProjectItem(entry) => {
            let Some(catalog) = &state.media_catalog else {
                return Err("Project media changed. Search again.".into());
            };
            let request_id = bridge
                .place_project_item(catalog, entry.item_id.clone())
                .map_err(SharedString::from)?;
            ui.set_status_message(format!("Placing {}…", entry.name).into());
            state.pending_placement = Some(PendingPlacement {
                peer_id: catalog.peer_id,
                request_id,
                deadline: Instant::now() + ACTION_TIMEOUT,
                recent: RecentItem {
                    item: SearchResult::ProjectItem(entry),
                    project_id: catalog.project_id.clone(),
                },
            });
        }
    }
    // Hide before transferring focus, since Premiere may already be processing the edit.
    hide_palette(ui, state);
    restore_premiere_focus(state.previous_hwnd);
    Ok(())
}

fn main() -> Result<(), slint::PlatformError> {
    // 1. Auto-install / enable the UXP bridge plugin in Premiere Pro & register Windows Startup (--watch)
    installer::ensure_installed();

    // 2. Single-instance check: if Premiere Spotlight is already running in the System Tray, wake it (only opens if Premiere is active) and exit
    if bridge::check_or_wake_existing_instance(true) {
        return Ok(());
    }

    #[cfg(windows)]
    {
        use slint::winit_030::winit::platform::windows::WindowAttributesExtWindows;
        use slint::winit_030::winit::window::WindowButtons;

        // Set the native style at creation so Windows never maps a captioned palette.
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .with_winit_window_attributes_hook(|attributes| {
                let attributes = if error_toast::CREATING_TOAST.with(|flag| flag.get()) {
                    attributes.with_active(false)
                } else {
                    attributes
                };
                attributes
                    .with_decorations(false)
                    .with_undecorated_shadow(false)
                    .with_transparent(false)
                    .with_resizable(false)
                    .with_enabled_buttons(WindowButtons::empty())
                    .with_skip_taskbar(true)
            })
            .select()?;
    }
    let ui = SpotlightWindow::new()?;
    let error_toast = Rc::new(ErrorToast::default());
    win_focus::init_palette_window(extract_window_hwnd(&ui));

    // Create native Windows System Tray icon (no taskbar icon)
    let tray = Rc::new(RefCell::new(SystemTray::new()));

    let state = Rc::new(RefCell::new(AppState {
        catalog: CatalogStore::new_with_defaults(),
        matched_items: Vec::new(),
        recent_items: Vec::new(),
        pending_effects: HashMap::new(),
        media_catalog: None,
        pending_placement: None,
        selected_abs_index: 0,
        scroll_offset: 0,
        is_visible: false,
        shown_at: None,
        saw_foreground: false,
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
                    BridgeEvent::ApplyResult(result) => {
                        EFFECT_RESULTS.with(|results| results.borrow_mut().push(result));
                    }
                    BridgeEvent::ProjectItemsChanged => {
                        MEDIA_DIRTY.with(|dirty| dirty.set(true));
                    }
                    BridgeEvent::PlacementResult(result) => {
                        PLACEMENT_QUEUE.with(|q| q.borrow_mut().push(result));
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

    // The header badge and Tab share the catalog switch.
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_catalog_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
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
        let error_toast = error_toast.clone();
        ui.on_activate_selected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.selected_abs_index;
                if let Err(message) = apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref) {
                    error_toast.show(message, st.previous_hwnd);
                }
            }
        });
    }

    // Mouse click on a row applies that row
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let error_toast = error_toast.clone();
        ui.on_activate_index(move |rel_idx| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.scroll_offset + (rel_idx.max(0) as usize);
                st.selected_abs_index = abs_idx;
                if let Err(message) = apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref) {
                    error_toast.show(message, st.previous_hwnd);
                }
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
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.window().on_close_requested(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                hide_and_restore_focus(&ui, &mut st);
            }
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    // Keep Ctrl+Space registered across minimize/restore. The handler checks
    // the current foreground window before opening Spotlight.
    let hotkey_manager =
        GlobalHotKeyManager::new().expect("Failed to initialize GlobalHotKeyManager");
    let ctrl_space = HotKey::new(Some(Modifiers::CONTROL), Code::Space);
    let ctrl_space_id = ctrl_space.id();
    if let Err(error) = hotkey_manager.register(ctrl_space) {
        eprintln!("Could not register Ctrl+Space: {error}");
        tray::mark_hotkey_unavailable();
    }
    let hotkey_pending = Arc::new(AtomicBool::new(false));
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let pending_flag = hotkey_pending.clone();
        HOTKEY_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if !pending_flag.swap(false, Ordering::AcqRel) {
                    return;
                }
                if let Some(ui) = ui_weak.upgrade() {
                    let mut st = state_ref.borrow_mut();
                    toggle_palette_from_hotkey(&ui, &mut st, &bridge_ref);
                }
            }));
        });
    }
    {
        let pending_flag = hotkey_pending.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.id == ctrl_space_id && event.state == HotKeyState::Pressed {
                // Capture WM_HOTKEY foreground rights immediately and wake the event loop without timer lag.
                win_focus::allow_current_process_foreground();
                pending_flag.store(true, Ordering::Release);
                let _ = slint::invoke_from_event_loop(|| {
                    HOTKEY_DISPATCH.with(|slot| {
                        if let Some(callback) = slot.borrow_mut().as_mut() {
                            callback();
                        }
                    });
                });
            }
        }));
    }
    let sync_palette_focus = {
        let state_ref = state.clone();
        Rc::new(move |ui: &SpotlightWindow| {
            let our_hwnd = extract_window_hwnd(ui);
            win_focus::init_palette_window(our_hwnd);
            let is_our_window = win_focus::is_palette_open_and_active(our_hwnd);

            let focus_outcome = {
                let mut st = state_ref.borrow_mut();
                evaluate_palette_focus(&mut st, is_our_window, Instant::now())
            };
            match focus_outcome {
                FocusOutcome::KeepVisible => {}
                FocusOutcome::RetryForeground => {
                    win_focus::force_foreground(our_hwnd);
                }
                FocusOutcome::HideLostFocus => {
                    let _ = ui.hide();
                }
            }
        })
    };
    {
        let ui_weak = ui.as_weak();
        let sync_fn = sync_palette_focus.clone();
        SYNC_FOCUS_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    sync_fn(&ui);
                }
            }));
        });
    }
    let _foreground_hook = win_focus::ForegroundHook::install(|| {
        let _ = slint::invoke_from_event_loop(|| {
            SYNC_FOCUS_DISPATCH.with(|slot| {
                if let Some(callback) = slot.borrow_mut().as_mut() {
                    callback();
                }
            });
        });
    });
    let _ = slint::invoke_from_event_loop(|| {
        SYNC_FOCUS_DISPATCH.with(|slot| {
            if let Some(callback) = slot.borrow_mut().as_mut() {
                callback();
            }
        });
    });

    // Poll focus state and system tray actions on a 20ms timer.
    let timer = Timer::default();
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let tray_ref = tray.clone();
        let bridge_ref = bridge.clone();
        let hotkey_pending = hotkey_pending.clone();
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
                    refresh_catalog_ui(&ui, &mut st, true);
                }
            });
            if MEDIA_DIRTY.with(|dirty| dirty.replace(false)) {
                let mut st = state_ref.borrow_mut();
                let catalog = match bridge_ref.media_catalog() {
                    Ok(catalog) => catalog,
                    Err(message) => {
                        if st.is_visible {
                            error_toast.show(message.into(), st.previous_hwnd);
                        }
                        None
                    }
                };
                let preserve_media =
                    st.media_catalog
                        .as_ref()
                        .zip(catalog.as_ref())
                        .is_some_and(|(old, new)| {
                            old.peer_id == new.peer_id
                                && old.project_id == new.project_id
                                && old.sequence_id == new.sequence_id
                        });
                let items = catalog
                    .as_ref()
                    .map(|catalog| catalog.items.clone())
                    .unwrap_or_default();
                st.catalog.set_project_items(items);
                st.media_catalog = catalog;
                refresh_catalog_ui(&ui, &mut st, preserve_media);
            }
            {
                let mut st = state_ref.borrow_mut();
                let effects = EFFECT_RESULTS.with(|q| std::mem::take(&mut *q.borrow_mut()));
                let mut recent_changed = false;
                for result in effects {
                    recent_changed |= complete_effect(&mut st, &result);
                    if !result.ok {
                        let message = if result.message.is_empty() {
                            "Premiere could not apply the effect.".into()
                        } else {
                            result.message.into()
                        };
                        error_toast.show(message, st.previous_hwnd);
                    } else if !result.message.is_empty() {
                        ui.set_status_message(format!("Premiere: {}", result.message).into());
                    }
                }
                if recent_changed {
                    refresh_catalog_ui(&ui, &mut st, true);
                }
                st.pending_effects.retain(|_, pending| Instant::now() < pending.deadline);
                let results = PLACEMENT_QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
                if let Some(message) = update_placement(&ui, &mut st, results) {
                    error_toast.show(message, st.previous_hwnd);
                }
            }

            // 3. Handle System Tray menu / click actions
            while let Some(action) = tray::poll_tray_action() {
                match action {
                    TrayAction::OpenSpotlight => {
                        let (_, unminimized_premiere_hwnd) =
                            win_focus::query_premiere_window_state();
                        if unminimized_premiere_hwnd != 0 {
                            let mut st = state_ref.borrow_mut();
                            st.previous_hwnd = unminimized_premiere_hwnd;
                            center_and_show(&ui, &mut st, &bridge_ref);
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
                    center_and_show(&ui, &mut st, &bridge_ref);
                }
            }

            // 5. Dispatch the shortcut if the wake callback hasn't handled it yet.
            if hotkey_pending.swap(false, Ordering::AcqRel) {
                let mut st = state_ref.borrow_mut();
                toggle_palette_from_hotkey(&ui, &mut st, &bridge_ref);
            }
            // 6. Dismiss Spotlight when it loses focus.
            sync_palette_focus(&ui);
        });
    }

    // Keep event loop alive while helper is active in the System Tray
    slint::run_event_loop_until_quit()?;
    Ok(())
}

thread_local! {
    static CATALOG_QUEUE: RefCell<Vec<Vec<EffectEntry>>> = const { RefCell::new(Vec::new()) };
    static MEDIA_DIRTY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PLACEMENT_QUEUE: RefCell<Vec<PlacementResult>> = const { RefCell::new(Vec::new()) };
    static EFFECT_RESULTS: RefCell<Vec<ApplyResult>> = const { RefCell::new(Vec::new()) };
    static SHOW_WINDOW_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static BRIDGE_STATUS_QUEUE: RefCell<Option<bool>> = const { RefCell::new(None) };
    static HOTKEY_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static SYNC_FOCUS_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::{
        software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
        Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    };
    use slint::{LogicalPosition, PhysicalSize, Rgb8Pixel};

    const WINDOW_WIDTH: i32 = 620;
    const WINDOW_HEIGHT: i32 = 407;

    struct TestPlatform(Rc<MinimalSoftwareWindow>);

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn error_toast_renders_a_separate_notification() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let (width, height) = (304, 54);
        adapter.set_size(PhysicalSize::new(width as u32, height as u32));
        let toast = Rc::new(ErrorToast::default());
        toast.show("Select at least one timeline clip first.".into(), 0);

        let mut pixels = vec![Rgb8Pixel::default(); width * height];
        assert!(adapter.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, width);
        }));
        assert_eq!(pixels[width * (height / 2) + 15].r, 0xca);
        assert!(pixels.chunks_exact(width).any(|row| {
            row[26..width - 12].iter().any(|pixel| pixel.g > 160)
        }));
    }

    #[test]
    fn accepted_actions_hide_immediately_and_errors_leave_visibility_alone() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let bridge = BridgeClients::new();
        let effect = SearchResult::Effect(EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        });
        let item = catalog::ProjectItemEntry {
            item_id: "clip".into(),
            name: "Clip".into(),
            bin_path: "Footage".into(),
            kind: ProjectItemKind::Media,
            offline: false,
        };
        let catalog = MediaCatalog {
            peer_id: 7,
            project_id: Some("project".into()),
            sequence_id: Some("sequence".into()),
            items: vec![item.clone()],
        };
        let mut state = AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![effect, SearchResult::ProjectItem(item)],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: Some(catalog.clone()),
            pending_placement: None,
            selected_abs_index: 0,
            scroll_offset: 0,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };
        show_palette(&ui, &mut state);
        for index in [0, 1] {
            let message = apply_entry_at(index, &ui, &mut state, &bridge).unwrap_err();
            assert!(state.is_visible);
            assert!(message.contains("bridge disconnected"));
        }

        state.catalog.set_project_items(catalog.items.clone());
        let requests = bridge.connect_test_peer(catalog);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        assert_eq!(state.pending_effects.len(), 1);
        assert!(requests.try_recv().unwrap().contains("apply_effect"));

        show_palette(&ui, &mut state);
        ui.set_active_catalog("project".into());
        ui.set_search_text("Clip".into());
        refresh_results_ui(&ui, &mut state);
        for success in [true, false] {
            apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
            assert!(!state.is_visible);
            assert!(requests.try_recv().unwrap().contains("place_project_item"));
            let request_id = state.pending_placement.as_ref().unwrap().request_id;
            if success {
                // A success reply must leave a manually reopened palette alone.
                show_palette(&ui, &mut state);
            }
            let error = update_placement(
                &ui,
                &mut state,
                vec![PlacementResult {
                    peer_id: 7,
                    request_id,
                    ok: success,
                    message: if success { "Placed Clip" } else { "Timeline changed" }.into(),
                }],
            );
            assert_eq!(state.is_visible, success);
            assert_eq!(
                error.as_deref(),
                if success { None } else { Some("Timeline changed") },
            );
            assert_eq!(ui.get_search_text().as_str(), "Clip");
            assert_eq!(state.recent_items.len(), 1);
        }
    }

    #[test]
    fn search_header_cycles_and_reopens_to_recent_items() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        ui.set_search_text("Take".into());
        ui.invoke_focus_search();
        let press_tab = || {
            window.dispatch_event(WindowEvent::KeyPressed { text: Key::Tab.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: Key::Tab.into() });
        };
        let click_badge = || {
            let position = LogicalPosition::new(32., 27.);
            let button = PointerEventButton::Left;
            window.dispatch_event(WindowEvent::PointerPressed { position, button });
            window.dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        let badge_color = || {
            let width = WINDOW_WIDTH as usize;
            let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
            window.request_redraw();
            assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
            pixels[21 * width + 20]
        };
        let all_color = badge_color();
        press_tab();
        let effects_color = badge_color();
        press_tab();
        let project_color = badge_color();
        assert_ne!(all_color, effects_color);
        assert_ne!(effects_color, project_color);
        assert_ne!(all_color, project_color);
        press_tab();
        for switch in [&press_tab as &dyn Fn(), &click_badge] {
            for expected in ["effects", "project", "all"] {
                switch();
                assert_eq!(ui.get_active_catalog().as_str(), expected);
                assert_eq!(ui.get_search_text().as_str(), "Take");
            }
        }
        click_badge();
        press_tab();
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        let mut state = AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: None,
            pending_placement: None,
            selected_abs_index: 0,
            scroll_offset: 0,
            is_visible: true,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };
        let effect = EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        };
        let pending = || PendingEffect {
            entry: effect.clone(),
            recipients: vec![7],
            deadline: Instant::now() + ACTION_TIMEOUT,
        };
        let ack = |request_id, ok| ApplyResult {
            peer_id: 7,
            request_id,
            ok,
            message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
        };
        state.pending_effects.insert(1, pending());
        assert!(!complete_effect(&mut state, &ack(Some(2), true)));
        assert!(!complete_effect(&mut state, &ack(Some(1), false)));
        assert!(state.recent_items.is_empty());
        state.pending_effects.insert(3, pending());
        state.pending_effects.insert(4, pending());
        assert!(!complete_effect(&mut state, &ack(None, true)));
        assert!(complete_effect(&mut state, &ack(Some(3), true)));
        let mut other_peer = ack(None, true);
        other_peer.peer_id = 8;
        assert!(!complete_effect(&mut state, &other_peer));
        assert!(complete_effect(&mut state, &ack(None, true)));
        assert!(!complete_effect(&mut state, &ack(None, true)));
        let mut both_peers = pending();
        both_peers.recipients.push(8);
        state.pending_effects.insert(5, both_peers);
        assert!(!complete_effect(&mut state, &ack(Some(5), false)));
        let mut success = ack(Some(5), true);
        success.peer_id = 8;
        assert!(complete_effect(&mut state, &success));
        ui.set_search_text(" take ".into());
        hide_and_restore_focus(&ui, &mut state);
        let bridge = BridgeClients::new();
        center_and_show(&ui, &mut state, &bridge);
        assert_eq!(ui.get_active_catalog().as_str(), "all");
        assert!(ui.get_search_text().is_empty());
        assert_eq!(window.size().height, WINDOW_HEIGHT as u32);
        assert!(matches!(state.matched_items.as_slice(),
            [SearchResult::Effect(entry)] if entry.name == "Gaussian Blur"));

        let item = catalog::ProjectItemEntry {
            item_id: "audio".into(),
            name: "achievement_sfx.mp3".into(),
            bin_path: "Audio".into(),
            kind: ProjectItemKind::Media,
            offline: false,
        };
        remember_item(&mut state, RecentItem {
            item: SearchResult::ProjectItem(item.clone()), project_id: Some("project".into()),
        });
        state.media_catalog = Some(MediaCatalog {
            peer_id: 1,
            project_id: Some("project".into()),
            sequence_id: Some("sequence".into()),
            items: vec![item],
        });
        refresh_results_ui(&ui, &mut state);
        assert!(matches!(state.matched_items.as_slice(),
            [SearchResult::ProjectItem(item), SearchResult::Effect(_)]
                if item.name == "achievement_sfx.mp3"));
        state.media_catalog.as_mut().unwrap().project_id = Some("different project".into());
        refresh_results_ui(&ui, &mut state);
        assert_eq!(state.matched_items.len(), 1);

        ui.set_search_text("".into());
        press_tab();
        press_tab();
        assert_eq!(ui.get_active_catalog().as_str(), "project");

        state.catalog.set_project_items((1..=8).map(|i| catalog::ProjectItemEntry {
            item_id: i.to_string(),
            name: format!("Project item {i}"),
            bin_path: "Footage / Interview".into(),
            kind: catalog::ProjectItemKind::Media,
            offline: false,
        }).collect());
        refresh_results_ui(&ui, &mut state);
        assert_eq!(ui.get_total_matches(), 8);
        click_badge();
        click_badge();
        click_badge();
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        ui.set_status_message("This project item can't be placed on the timeline.".into());

        let width = WINDOW_WIDTH as usize;
        let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
        window.request_redraw();
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        for row in 0..8 {
            let top = (61 + row * 38) * width;
            let bottom = top + 36 * width;
            assert!(pixels[top..bottom].iter().any(|pixel| pixel.r > 170), "row {row} should render its label");
        }
        assert!(pixels[371 * width..].iter().any(|pixel| pixel.r > 130), "the footer should render its text");
        let height = WINDOW_HEIGHT as usize;
        let outline = Rgb8Pixel { r: 0x36, g: 0x39, b: 0x52 };
        for edge in [
            [width / 2, width + width / 2, 2 * width + width / 2],
            std::array::from_fn(|i| (height - 1 - i) * width + width / 2),
            std::array::from_fn(|i| height / 2 * width + i),
            std::array::from_fn(|i| height / 2 * width + width - 1 - i),
        ] {
            assert!(edge.iter().any(|&i| pixels[i] == outline), "outline missing at a palette edge");
        }

        // Escape should close the palette when search has focus and after clicking elsewhere inside the palette.
        let close_count = Rc::new(std::cell::Cell::new(0u32));
        {
            let close_count = close_count.clone();
            ui.on_close_requested(move || {
                close_count.set(close_count.get() + 1);
            });
        }
        ui.invoke_focus_search();
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Escape.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Escape.into() });
        assert_eq!(close_count.get(), 1);
        let footer_pos = LogicalPosition::new(300., 390.);
        window.dispatch_event(WindowEvent::PointerPressed {
            position: footer_pos,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: footer_pos,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Escape.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Escape.into() });
        assert_eq!(close_count.get(), 2);

        // Clicking outside (including back onto Premiere) hides once activation succeeded or grace expires.
        let shown_at = Instant::now();
        state.is_visible = true;
        state.shown_at = Some(shown_at);
        state.saw_foreground = false;
        assert!(matches!(
            evaluate_palette_focus(&mut state, false, shown_at + Duration::from_millis(20)),
            FocusOutcome::RetryForeground
        ));
        assert!(state.is_visible);
        assert!(matches!(
            evaluate_palette_focus(&mut state, true, shown_at + Duration::from_millis(40)),
            FocusOutcome::KeepVisible
        ));
        assert!(state.saw_foreground);
        assert!(matches!(
            evaluate_palette_focus(&mut state, false, shown_at + Duration::from_millis(60)),
            FocusOutcome::HideLostFocus
        ));
        assert!(!state.is_visible);

        state.is_visible = true;
        state.shown_at = Some(shown_at);
        state.saw_foreground = false;
        assert!(matches!(
            evaluate_palette_focus(&mut state, false, shown_at + ACTIVATION_GRACE),
            FocusOutcome::HideLostFocus
        ));
        assert!(!state.is_visible);

        // Reopening after focus-loss dismissal or hide_and_restore_focus must take one toggle.
        toggle_palette_from_hotkey_for_fg(&ui, &mut state, &bridge, 4242, true);
        assert!(state.is_visible);
        assert_eq!(state.previous_hwnd, 4242);
        assert!(state.shown_at.is_some());

        // Closing via hide_and_restore_focus clears visibility, then one toggle reopens it.
        state.previous_hwnd = 0;
        hide_and_restore_focus(&ui, &mut state);
        assert!(!state.is_visible);
        assert!(state.shown_at.is_none());
        assert!(!state.saw_foreground);

        toggle_palette_from_hotkey_for_fg(&ui, &mut state, &bridge, 4242, true);
        assert!(state.is_visible);
        assert_eq!(state.previous_hwnd, 4242);
        assert!(state.shown_at.is_some());

        // If the OS minimized/hid the palette (palette_actively_open = false) while Premiere is active,
        // one toggle must reopen and re-anchor to Premiere rather than silently hiding.
        state.previous_hwnd = 1111;
        state.shown_at = None;
        toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 4242, true, false);
        assert!(state.is_visible);
        assert_eq!(state.previous_hwnd, 4242);
        assert!(state.shown_at.is_some());

        // Pressing toggle while the palette is actually visible and active closes it cleanly.
        toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 9999, false, true);
        assert!(!state.is_visible);
        assert!(state.shown_at.is_none());

        // Stale is_visible while Premiere is inactive clears state cleanly.
        state.is_visible = true;
        toggle_palette_from_hotkey_with_focus(&ui, &mut state, &bridge, 9999, false, false);
        assert!(!state.is_visible);
    }
}
