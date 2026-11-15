#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod catalog;
mod debug_log;
mod error_toast;
mod installer;
mod settings;
mod tray;
mod win_focus;

use bridge::{
    ApplyResult, BridgeClients, BridgeEvent, EffectControlsShortcut, MediaCatalog, PlacementResult,
};
use catalog::{CatalogStore, EffectEntry, ProjectItemEntry, ProjectItemKind, SearchResult};
use debug_log::{DebugLogController, LogKind};
use error_toast::ErrorToast;
use global_hotkey::{
    hotkey::HotKey,
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use settings::{AlternateModifier, AppSettings, PaletteShortcutAction};
use slint::{
    Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
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
    pending_source_open: Option<PendingSourceOpen>,
    active_bin_path: Option<String>,
    settings: AppSettings,
    selected_abs_index: usize,
    scroll_offset: usize,
    rendered_scroll_offset: Option<usize>,
    results_dirty: bool,
    is_visible: bool,
    shown_at: Option<Instant>,
    saw_foreground: bool,
    previous_hwnd: isize,
}

struct PendingEffect {
    entry: Arc<EffectEntry>,
    recipients: Vec<usize>,
    deadline: Instant,
    open_effect_controls: bool,
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

struct PendingSourceOpen {
    peer_id: usize,
    request_id: u64,
    deadline: Instant,
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
                    .map(Arc::new)
                    .map(SearchResult::ProjectItem)
            }
        })
        .collect()
}

/// Returns `Some(open_effect_controls)` if an effect apply just completed successfully.
fn complete_effect(state: &mut AppState, result: &ApplyResult) -> Option<bool> {
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
                _ => return None,
            }
        }
        None => return None,
    };
    let pending = state.pending_effects.get_mut(&request_id)?;
    let index = pending.recipients.iter().position(|peer| *peer == result.peer_id)?;
    if Instant::now() >= pending.deadline {
        return None;
    }
    if !result.ok {
        pending.recipients.swap_remove(index);
        if pending.recipients.is_empty() {
            state.pending_effects.remove(&request_id);
        }
        return None;
    }
    let completed = state.pending_effects.remove(&request_id).unwrap();
    let entry = completed.entry;
    state.catalog.record_usage(&entry.match_name);
    remember_item(
        state,
        RecentItem {
            item: SearchResult::Effect(entry),
            project_id: None,
        },
    );
    Some(completed.open_effect_controls)
}

fn sync_bin_ui(ui: &SpotlightWindow, state: &AppState) {
    ui.set_active_bin(state.active_bin_path.as_deref().unwrap_or("").into());
}

fn clear_bin_scope(ui: &SpotlightWindow, state: &mut AppState) {
    state.active_bin_path = None;
    sync_bin_ui(ui, state);
}

fn enter_bin(entry: &ProjectItemEntry, ui: &SpotlightWindow, state: &mut AppState) {
    let Some(bin_path) = entry.child_bin_path() else {
        return;
    };
    state.active_bin_path = Some(bin_path.clone());
    sync_bin_ui(ui, state);
    ui.set_active_catalog("project".into());
    ui.set_search_text(SharedString::from(""));
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
    ui.set_status_message(format!("Browsing {} · Backspace to go up", bin_path).into());
    ui.invoke_focus_search();
}

fn step_out_bin(ui: &SpotlightWindow, state: &mut AppState) {
    let Some(current) = state.active_bin_path.take() else {
        return;
    };
    state.active_bin_path = current
        .rsplit_once(" / ")
        .map(|(parent, _)| parent.to_string());
    sync_bin_ui(ui, state);
    ui.set_search_text(SharedString::from(""));
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
    if let Some(parent) = &state.active_bin_path {
        ui.set_status_message(format!("Browsing {} · Backspace to go up", parent).into());
    } else {
        ui.set_status_message("Showing all project items".into());
    }
    ui.invoke_focus_search();
}

fn refresh_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    let query = ui.get_search_text();
    let catalog = ui.get_active_catalog();
    let show_history =
        state.active_bin_path.is_none() && catalog.as_str() == "all" && query.trim().is_empty();
    ui.set_show_recent_items(show_history);
    let started = Instant::now();
    state.matched_items = if show_history {
        recent_results(state)
    } else {
        state.catalog.search_in_bin(
            query.as_str(),
            catalog.as_str(),
            state.active_bin_path.as_deref(),
        )
    };
    let elapsed = started.elapsed();
    let total = state.matched_items.len();
    if state.is_visible {
        let (effects_count, media_count) = state.catalog.counts();
        debug_log::record_search_timing(
            elapsed,
            query.as_str(),
            catalog.as_str(),
            total,
            effects_count + media_count,
        );
    }
    ui.set_total_matches(total as i32);
    state.results_dirty = true;

    if total == 0 {
        state.selected_abs_index = 0;
        state.scroll_offset = 0;
        state.rendered_scroll_offset = None;
        state.results_dirty = false;
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
        state.rendered_scroll_offset = None;
        state.results_dirty = false;
        ui.set_results(ModelRc::new(VecModel::from(Vec::<EffectItemUI>::new())));
        ui.set_selected_index(0);
        return;
    }

    if state.selected_abs_index < state.scroll_offset {
        state.scroll_offset = state.selected_abs_index;
    } else if state.selected_abs_index >= state.scroll_offset + VISIBLE_ROWS {
        state.scroll_offset = state.selected_abs_index + 1 - VISIBLE_ROWS;
    }

    let rel_idx = (state.selected_abs_index - state.scroll_offset) as i32;
    if !state.results_dirty && state.rendered_scroll_offset == Some(state.scroll_offset) {
        ui.set_selected_index(rel_idx);
        return;
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

    ui.set_results(ModelRc::new(VecModel::from(slice)));
    ui.set_selected_index(rel_idx);
    state.rendered_scroll_offset = Some(state.scroll_offset);
    state.results_dirty = false;
}

fn extract_window_hwnd(ui: &SpotlightWindow) -> isize {
    win_focus::window_hwnd(ui.window())
}

fn stop_shortcut_recording(ui: &SpotlightWindow, rec_flag: Option<&AtomicBool>) {
    ui.set_recording_hotkey(false);
    ui.set_recording_target("".into());
    if let Some(flag) = rec_flag {
        flag.store(false, Ordering::Release);
    }
}

fn center_and_show(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients) {
    stop_shortcut_recording(ui, None);
    ui.set_settings_open(false);
    ui.set_settings_feedback("".into());
    ui.set_active_catalog("all".into());
    ui.set_search_text(SharedString::from(""));
    clear_bin_scope(ui, state);
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
    win_focus::set_palette_visible(true);
    start_focus_grace_timer();
    if !ui.get_settings_open() {
        ui.invoke_focus_search();
    }
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
    win_focus::set_palette_visible(false);
    stop_focus_grace_timer();
    stop_shortcut_recording(ui, None);
    ui.set_settings_open(false);
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

fn update_source_open(
    ui: &SpotlightWindow,
    state: &mut AppState,
    results: Vec<PlacementResult>,
) -> Option<SharedString> {
    let pending = state.pending_source_open.as_ref()?;
    if let Some(result) = results.into_iter().find(|result| {
        result.peer_id == pending.peer_id && result.request_id == pending.request_id
    }) {
        state.pending_source_open = None;
        if result.ok {
            ui.set_status_message(result.message.into());
        } else {
            return Some(if result.message.is_empty() {
                "Premiere could not open the item in the Source Monitor.".into()
            } else {
                result.message.into()
            });
        }
    } else if Instant::now() >= pending.deadline {
        state.pending_source_open = None;
        return Some("Source Monitor didn't respond. Check Premiere.".into());
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
        debug_log::log_event(
            LogKind::Focus,
            "HOTKEY",
            &format!("{} -> hide palette", state.settings.hotkey),
        );
        hide_and_restore_focus(ui, state);
        return;
    }
    if is_premiere_fg {
        debug_log::log_event(
            LogKind::Focus,
            "HOTKEY",
            &format!("{} -> show palette", state.settings.hotkey),
        );
        state.previous_hwnd = fg_hwnd;
        center_and_show(ui, state, bridge);
        return;
    }
    if state.is_visible {
        debug_log::log_event(
            LogKind::Focus,
            "HOTKEY",
            &format!("{} -> clear stale visibility", state.settings.hotkey),
        );
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
    let open_effect_controls =
        settings::resolve_open_effect_controls(state.settings.open_effect_controls_by_default, false);
    apply_entry_with_options(abs_index, ui, state, bridge, open_effect_controls)
}

fn apply_entry_with_options(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    open_effect_controls: bool,
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
                    open_effect_controls,
                },
            );
            schedule_action_deadlines(state);
        }
        SearchResult::ProjectItem(entry) => {
            if entry.kind == ProjectItemKind::Bin {
                enter_bin(&entry, ui, state);
                return Ok(());
            }
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
            schedule_action_deadlines(state);
        }
    }
    // Hide before transferring focus, since Premiere may already be processing the edit.
    hide_palette(ui, state);
    restore_premiere_focus(state.previous_hwnd);
    Ok(())
}

fn shift_activate_at(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<(), SharedString> {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return Ok(());
    };
    match chosen {
        SearchResult::Effect(_) => {
            let open_effect_controls = settings::resolve_open_effect_controls(
                state.settings.open_effect_controls_by_default,
                true,
            );
            apply_entry_with_options(abs_index, ui, state, bridge, open_effect_controls)
        }
        SearchResult::ProjectItem(entry) => {
            let Some(catalog) = &state.media_catalog else {
                return Err("Project media changed. Search again.".into());
            };
            let request_id = bridge
                .open_in_source_monitor(catalog, entry.item_id.clone())
                .map_err(SharedString::from)?;
            ui.set_status_message(format!("Opening {}…", entry.name).into());
            state.pending_source_open = Some(PendingSourceOpen {
                peer_id: catalog.peer_id,
                request_id,
                deadline: Instant::now() + ACTION_TIMEOUT,
            });
            schedule_action_deadlines(state);
            hide_palette(ui, state);
            restore_premiere_focus(state.previous_hwnd);
            Ok(())
        }
    }
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
                let non_activating = error_toast::CREATING_TOAST.with(|flag| flag.get())
                    || debug_log::CREATING_DEBUG_WINDOW.with(|flag| flag.get());
                let attributes = if non_activating {
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
    let debug_log_ctrl = Rc::new(DebugLogController::default());
    win_focus::init_palette_window(extract_window_hwnd(&ui));
    debug_log::log_event(LogKind::Info, "INIT", "Premiere Spotlight helper started");

    let initial_settings = settings::load_settings();

    // Create native Windows System Tray icon (no taskbar icon)
    let tray = Rc::new(RefCell::new(SystemTray::new()));

    let state = Rc::new(RefCell::new(AppState {
        catalog: CatalogStore::new_with_defaults(),
        matched_items: Vec::new(),
        recent_items: Vec::new(),
        pending_effects: HashMap::new(),
        media_catalog: None,
        pending_placement: None,
        pending_source_open: None,
        active_bin_path: None,
        settings: initial_settings.clone(),
        selected_abs_index: 0,
        scroll_offset: 0,
        rendered_scroll_offset: None,
        results_dirty: true,
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
        let state_ref = state.clone();
        let tray_ref = tray.clone();
        let bridge_ref = bridge.clone();
        let error_toast = error_toast.clone();
        BRIDGE_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |event: BridgeEvent| {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                match event {
                    BridgeEvent::ClientConnected => {
                        let hotkey = state_ref.borrow().settings.hotkey.clone();
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(format!(
                            "Connected to Premiere Pro · {hotkey}"
                        )));
                        if let Some(ref t) = *tray_ref.borrow() {
                            t.set_connected(true);
                        }
                    }
                    BridgeEvent::ClientDisconnected => {
                        ui.set_bridge_connected(false);
                        ui.set_status_message(SharedString::from(
                            "Premiere disconnected · Standalone catalog active",
                        ));
                        if let Some(ref t) = *tray_ref.borrow() {
                            t.set_connected(false);
                        }
                    }
                    BridgeEvent::CatalogReceived(items) => {
                        let hotkey = state_ref.borrow().settings.hotkey.clone();
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(format!(
                            "Premiere synced ({} effects) · {hotkey}",
                            items.len()
                        )));
                        if let Some(ref t) = *tray_ref.borrow() {
                            t.set_connected(true);
                        }
                        let mut st = state_ref.borrow_mut();
                        st.catalog.set_catalog(items);
                        refresh_catalog_ui(&ui, &mut st, true);
                    }
                    BridgeEvent::ApplyResult(result) => {
                        let mut st = state_ref.borrow_mut();
                        let completed = complete_effect(&mut st, &result);
                        if !result.ok {
                            let message: SharedString = if result.message.is_empty() {
                                "Premiere could not apply the effect.".into()
                            } else {
                                result.message.into()
                            };
                            debug_log::log_event(LogKind::Error, "APPLY", message.as_str());
                            error_toast.show(message, st.previous_hwnd);
                        } else if !result.message.is_empty() {
                            debug_log::log_event(LogKind::Success, "APPLY", result.message.as_str());
                            ui.set_status_message(format!("Premiere: {}", result.message).into());
                        }
                        if let Some(open_effect_controls) = completed {
                            refresh_catalog_ui(&ui, &mut st, true);
                            if open_effect_controls {
                                match bridge::read_effect_controls_shortcut() {
                                    EffectControlsShortcut::Bound(shortcut) => {
                                        if win_focus::open_effect_controls(
                                            st.previous_hwnd,
                                            shortcut,
                                        ) {
                                            debug_log::log_event(
                                                LogKind::Focus,
                                                "EFFECT_CONTROLS",
                                                "Opened Effect Controls panel",
                                            );
                                        } else {
                                            debug_log::log_event(
                                                LogKind::Error,
                                                "EFFECT_CONTROLS",
                                                "Skipped opening Effect Controls (Premiere not active)",
                                            );
                                        }
                                    }
                                    EffectControlsShortcut::Unbound => {
                                        let message: SharedString =
                                            "Effect applied, but no shortcut is bound to Effect Controls in Premiere Pro."
                                                .into();
                                        debug_log::log_event(
                                            LogKind::Error,
                                            "EFFECT_CONTROLS",
                                            message.as_str(),
                                        );
                                        error_toast.show(message, st.previous_hwnd);
                                    }
                                }
                            }
                        }
                        st.pending_effects
                            .retain(|_, pending| Instant::now() < pending.deadline);
                        schedule_action_deadlines(&st);
                    }
                    BridgeEvent::ProjectItemsChanged => {
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
                        let preserve_media = st
                            .media_catalog
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
                        if !preserve_media
                            || st
                                .active_bin_path
                                .as_deref()
                                .is_some_and(|path| !st.catalog.contains_bin_path(path))
                        {
                            clear_bin_scope(&ui, &mut st);
                        }
                        refresh_catalog_ui(&ui, &mut st, preserve_media);
                    }
                    BridgeEvent::PlacementResult(result) => {
                        let mut st = state_ref.borrow_mut();
                        if let Some(message) = update_placement(&ui, &mut st, vec![result]) {
                            debug_log::log_event(LogKind::Error, "PLACE", message.as_str());
                            error_toast.show(message, st.previous_hwnd);
                        }
                        schedule_action_deadlines(&st);
                    }
                    BridgeEvent::SourceMonitorResult(result) => {
                        let mut st = state_ref.borrow_mut();
                        if let Some(message) = update_source_open(&ui, &mut st, vec![result]) {
                            debug_log::log_event(LogKind::Error, "SOURCE", message.as_str());
                            error_toast.show(message, st.previous_hwnd);
                        }
                        schedule_action_deadlines(&st);
                    }
                    BridgeEvent::ShowWindowRequested => {
                        let fg_hwnd = win_focus::get_foreground_hwnd();
                        if win_focus::is_premiere_active_and_unminimized(fg_hwnd) {
                            let mut st = state_ref.borrow_mut();
                            st.previous_hwnd = fg_hwnd;
                            center_and_show(&ui, &mut st, &bridge_ref);
                        }
                    }
                }
            }));
        });
        bridge.start_server(move |event| {
            let _ = slint::invoke_from_event_loop(move || {
                BRIDGE_DISPATCH.with(|slot| {
                    if let Some(callback) = slot.borrow_mut().as_mut() {
                        callback(event);
                    }
                });
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
                if ui.get_active_catalog().as_str() != "project" {
                    clear_bin_scope(&ui, &mut st);
                }
                st.selected_abs_index = 0;
                st.scroll_offset = 0;
                refresh_results_ui(&ui, &mut st);
            }
        });
    }

    // Step up one folder level from an active bin drill-down.
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_step_out_bin(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                step_out_bin(&ui, &mut st);
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

    // Shift+Enter / Shift+Click applies effects & opens Effect Controls, or opens project items in Source Monitor
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let error_toast = error_toast.clone();
        ui.on_shift_activate_selected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.selected_abs_index;
                if let Err(message) = shift_activate_at(abs_idx, &ui, &mut st, &bridge_ref) {
                    error_toast.show(message, st.previous_hwnd);
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let error_toast = error_toast.clone();
        ui.on_shift_activate_index(move |rel_idx| {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.scroll_offset + (rel_idx.max(0) as usize);
                st.selected_abs_index = abs_idx;
                sync_viewport_slice(&ui, &mut st);
                if let Err(message) = shift_activate_at(abs_idx, &ui, &mut st, &bridge_ref) {
                    error_toast.show(message, st.previous_hwnd);
                }
            }
        });
    }

    {
        let state_ref = state.clone();
        let debug_ctrl = debug_log_ctrl.clone();
        ui.on_toggle_debug_log(move || {
            let anchor = state_ref.borrow().previous_hwnd;
            debug_ctrl.toggle(anchor);
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

    const NO_HOTKEY_ID: u32 = u32::MAX;

    struct HotkeyController {
        manager: GlobalHotKeyManager,
        registered: Option<(HotKey, String)>,
        active_id: Arc<AtomicU32>,
        available: bool,
    }

    impl HotkeyController {
        fn new(initial_spec: &str) -> (Self, Option<String>) {
            let manager =
                GlobalHotKeyManager::new().expect("Failed to initialize GlobalHotKeyManager");
            let mut ctrl = Self {
                manager,
                registered: None,
                active_id: Arc::new(AtomicU32::new(NO_HOTKEY_ID)),
                available: false,
            };
            let err = match settings::parse_hotkey_spec(initial_spec) {
                Ok((hotkey, label)) => match ctrl.apply(hotkey, label.clone()) {
                    Ok(()) => None,
                    Err(msg) => {
                        eprintln!("Could not register {label}: {msg}");
                        Some(msg)
                    }
                },
                Err(msg) => Some(msg),
            };
            (ctrl, err)
        }

        fn apply(&mut self, hotkey: HotKey, label: String) -> Result<(), String> {
            if self.available
                && self
                    .registered
                    .as_ref()
                    .is_some_and(|(old, _)| *old == hotkey)
            {
                return Ok(());
            }

            let previous = self.registered.take();
            let was_available = self.available;
            if let Some((old_hk, _)) = previous {
                if was_available {
                    let _ = self.manager.unregister(old_hk);
                }
            }

            match self.manager.register(hotkey) {
                Ok(()) => {
                    self.active_id.store(hotkey.id(), Ordering::Release);
                    self.registered = Some((hotkey, label));
                    self.available = true;
                    Ok(())
                }
                Err(error) => {
                    if let Some((old_hk, old_label)) = previous {
                        if was_available && self.manager.register(old_hk).is_ok() {
                            self.active_id.store(old_hk.id(), Ordering::Release);
                            self.registered = Some((old_hk, old_label));
                            self.available = true;
                        } else {
                            self.active_id.store(NO_HOTKEY_ID, Ordering::Release);
                            self.available = false;
                        }
                    } else {
                        self.active_id.store(NO_HOTKEY_ID, Ordering::Release);
                        self.available = false;
                    }
                    Err(format!(
                        "{label} is already in use by Windows or another app ({error})."
                    ))
                }
            }
        }
    }

    fn modifier_label(modifier: AlternateModifier) -> &'static str {
        match modifier {
            AlternateModifier::Shift => "Shift",
            AlternateModifier::Ctrl => "Ctrl",
            AlternateModifier::Alt => "Alt",
        }
    }

    fn palette_shortcut_slot_mut<'a>(
        settings: &'a mut AppSettings,
        target: &str,
    ) -> Option<(&'static str, &'a mut String)> {
        match target {
            "settings" => Some(("Open Settings shortcut", &mut settings.settings_hotkey)),
            "catalog_all" => Some(("All catalog shortcut", &mut settings.catalog_all_hotkey)),
            "catalog_effects" => {
                Some(("Effects catalog shortcut", &mut settings.catalog_effects_hotkey))
            }
            "catalog_project" => {
                Some(("Project catalog shortcut", &mut settings.catalog_project_hotkey))
            }
            _ => None,
        }
    }

    fn sync_settings_ui(
        ui: &SpotlightWindow,
        settings: &AppSettings,
        hotkey_available: bool,
        tray: &Rc<RefCell<Option<SystemTray>>>,
    ) {
        ui.set_hotkey_label(settings.hotkey.clone().into());
        ui.set_open_effect_controls_default(settings.open_effect_controls_by_default);
        ui.set_alternate_modifier(settings.alternate_modifier.as_str().into());
        ui.set_settings_hotkey_label(settings.settings_hotkey.clone().into());
        ui.set_catalog_all_hotkey_label(settings.catalog_all_hotkey.clone().into());
        ui.set_catalog_effects_hotkey_label(settings.catalog_effects_hotkey.clone().into());
        ui.set_catalog_project_hotkey_label(settings.catalog_project_hotkey.clone().into());
        ui.set_config_path_display(settings::config_path().to_string_lossy().into_owned().into());
        if !ui.get_bridge_connected() {
            ui.set_status_message(format!("Ready · {}", settings.hotkey).into());
        }
        tray::set_hotkey_status(&settings.hotkey, hotkey_available);
        tray::set_default_effect_controls(settings.open_effect_controls_by_default);
        if let Some(ref t) = *tray.borrow() {
            t.refresh_tooltip();
        }
    }

    fn save_and_sync_settings(
        ui: &SpotlightWindow,
        settings: &AppSettings,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
        tray: &Rc<RefCell<Option<SystemTray>>>,
        success_msg: String,
    ) -> Result<(), String> {
        let available = hotkey_ctrl.borrow().available;
        let result = match settings::save_settings(settings) {
            Ok(()) => {
                ui.set_settings_feedback_error(false);
                ui.set_settings_feedback(success_msg.into());
                Ok(())
            }
            Err(err) => {
                ui.set_settings_feedback_error(true);
                ui.set_settings_feedback(err.clone().into());
                Err(err)
            }
        };
        sync_settings_ui(ui, settings, available, tray);
        result
    }

    fn apply_hotkey_update(
        ui: &SpotlightWindow,
        state: &mut AppState,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
        tray: &Rc<RefCell<Option<SystemTray>>>,
        hotkey: HotKey,
        label: String,
    ) -> Result<(), String> {
        if let Some(conflict) = settings::shortcut_conflict("launch", &label, &state.settings) {
            let available = hotkey_ctrl.borrow().available;
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(conflict.clone().into());
            sync_settings_ui(ui, &state.settings, available, tray);
            return Err(conflict);
        }
        let result = hotkey_ctrl.borrow_mut().apply(hotkey, label.clone());
        let available = hotkey_ctrl.borrow().available;
        match result {
            Ok(()) => {
                state.settings.hotkey = label.clone();
                save_and_sync_settings(
                    ui,
                    &state.settings,
                    hotkey_ctrl,
                    tray,
                    format!("Saved shortcut: {label}"),
                )
            }
            Err(err) => {
                ui.set_settings_feedback_error(true);
                ui.set_settings_feedback(err.clone().into());
                sync_settings_ui(ui, &state.settings, available, tray);
                Err(err)
            }
        }
    }

    fn toggle_default_effect_controls_setting(
        ui: &SpotlightWindow,
        state: &mut AppState,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
        tray: &Rc<RefCell<Option<SystemTray>>>,
    ) {
        state.settings.open_effect_controls_by_default =
            !state.settings.open_effect_controls_by_default;
        let msg = if state.settings.open_effect_controls_by_default {
            "Enter now applies effects and opens Effect Controls.".to_string()
        } else {
            format!(
                "{}+Enter opens Effect Controls (default restored).",
                modifier_label(state.settings.alternate_modifier)
            )
        };
        let _ = save_and_sync_settings(ui, &state.settings, hotkey_ctrl, tray, msg);
    }

    fn reload_settings_live(
        ui: &SpotlightWindow,
        state: &mut AppState,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
        tray: &Rc<RefCell<Option<SystemTray>>>,
    ) {
        let loaded = settings::load_settings();
        state.settings.open_effect_controls_by_default = loaded.open_effect_controls_by_default;
        state.settings.alternate_modifier = loaded.alternate_modifier;
        state.settings.settings_hotkey = loaded.settings_hotkey;
        state.settings.catalog_all_hotkey = loaded.catalog_all_hotkey;
        state.settings.catalog_effects_hotkey = loaded.catalog_effects_hotkey;
        state.settings.catalog_project_hotkey = loaded.catalog_project_hotkey;

        let mut error_msg = None;
        if let Ok((hotkey, label)) = settings::parse_hotkey_spec(&loaded.hotkey) {
            if let Some(conflict) = settings::shortcut_conflict("launch", &label, &state.settings) {
                error_msg = Some(conflict);
            } else if let Err(err) = hotkey_ctrl.borrow_mut().apply(hotkey, label.clone()) {
                error_msg = Some(err);
            } else {
                state.settings.hotkey = label;
            }
        }
        if let Some(err) = error_msg {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(err.into());
        } else {
            ui.set_settings_feedback_error(false);
            ui.set_settings_feedback("Reloaded settings from config.json".into());
        }
        let available = hotkey_ctrl.borrow().available;
        sync_settings_ui(ui, &state.settings, available, tray);
    }

    fn toggle_settings_view(ui: &SpotlightWindow, rec_flag: &AtomicBool) {
        let next = !ui.get_settings_open();
        stop_shortcut_recording(ui, Some(rec_flag));
        ui.set_settings_feedback("".into());
        ui.set_settings_open(next);
        if !next {
            ui.invoke_focus_search();
        }
    }

    fn jump_to_catalog(ui: &SpotlightWindow, state: &mut AppState, target: &str) {
        if ui.get_active_catalog().as_str() == target {
            return;
        }
        ui.set_active_catalog(target.into());
        if target != "project" {
            clear_bin_scope(ui, state);
        }
        state.selected_abs_index = 0;
        state.scroll_offset = 0;
        refresh_results_ui(ui, state);
    }

    fn open_config_file_in_editor(state: &AppState) {
        let path = settings::config_path();
        if !path.exists() {
            let _ = settings::save_settings(&state.settings);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            let _ = std::process::Command::new("cmd")
                .args(["/C", "start", "", &path.to_string_lossy()])
                .creation_flags(CREATE_NO_WINDOW)
                .spawn();
        }
    }

    let (hotkey_ctrl_raw, initial_hotkey_err) = HotkeyController::new(&initial_settings.hotkey);
    let hotkey_id_ref = hotkey_ctrl_raw.active_id.clone();
    let initial_hotkey_available = hotkey_ctrl_raw.available;
    let hotkey_ctrl = Rc::new(RefCell::new(hotkey_ctrl_raw));
    let recording_hotkey_flag = Arc::new(AtomicBool::new(false));
    {
        let st = state.borrow();
        sync_settings_ui(&ui, &st.settings, initial_hotkey_available, &tray);
        if let Some(err) = initial_hotkey_err {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(err.into());
        }
    }

    {
        let ui_weak = ui.as_weak();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_toggle_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                toggle_settings_view(&ui, &rec_flag);
            }
        });
    }
    {
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_hotkey_recording_changed(move |active| {
            rec_flag.store(active, Ordering::Release);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_capture_shortcut(move |target, text, ctrl, alt, shift, meta| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let target_str = target.as_str();
            if target_str == "launch" {
                match settings::hotkey_from_slint_event(text.as_str(), ctrl, alt, shift, meta) {
                    Ok(None) => {}
                    Ok(Some((hotkey, label))) => {
                        stop_shortcut_recording(&ui, Some(&rec_flag));
                        let mut st = state_ref.borrow_mut();
                        let _ = apply_hotkey_update(&ui, &mut st, &hk_ctrl, &tray_ref, hotkey, label);
                    }
                    Err(msg) => {
                        ui.set_settings_feedback_error(true);
                        ui.set_settings_feedback(msg.into());
                    }
                }
                return;
            }
            match settings::palette_hotkey_from_slint_event(text.as_str(), ctrl, alt, shift, meta) {
                Ok(None) => {}
                Ok(Some((_hotkey, label))) => {
                    stop_shortcut_recording(&ui, Some(&rec_flag));
                    let mut st = state_ref.borrow_mut();
                    if let Some(conflict) =
                        settings::shortcut_conflict(target_str, &label, &st.settings)
                    {
                        ui.set_settings_feedback_error(true);
                        ui.set_settings_feedback(conflict.into());
                        return;
                    }
                    if let Some((_, slot)) = palette_shortcut_slot_mut(&mut st.settings, target_str)
                    {
                        *slot = label.clone();
                        let _ = save_and_sync_settings(
                            &ui,
                            &st.settings,
                            &hk_ctrl,
                            &tray_ref,
                            format!("Saved shortcut: {label}"),
                        );
                    }
                }
                Err(msg) => {
                    ui.set_settings_feedback_error(true);
                    ui.set_settings_feedback(msg.into());
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_clear_shortcut(move |target| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            stop_shortcut_recording(&ui, Some(&rec_flag));
            let mut st = state_ref.borrow_mut();
            if let Some((slot_name, slot)) =
                palette_shortcut_slot_mut(&mut st.settings, target.as_str())
            {
                slot.clear();
                let _ = save_and_sync_settings(
                    &ui,
                    &st.settings,
                    &hk_ctrl,
                    &tray_ref,
                    format!("Cleared {slot_name}."),
                );
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_reset_default_hotkey(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            stop_shortcut_recording(&ui, Some(&rec_flag));
            if let Ok((hotkey, label)) = settings::parse_hotkey_spec(settings::DEFAULT_HOTKEY) {
                let mut st = state_ref.borrow_mut();
                let _ = apply_hotkey_update(&ui, &mut st, &hk_ctrl, &tray_ref, hotkey, label);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        ui.on_set_alternate_modifier(move |value| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let next = AlternateModifier::from_setting(value.as_str());
            let mut st = state_ref.borrow_mut();
            if st.settings.alternate_modifier == next {
                return;
            }
            st.settings.alternate_modifier = next;
            let _ = save_and_sync_settings(
                &ui,
                &st.settings,
                &hk_ctrl,
                &tray_ref,
                format!("Alternate action modifier set to {}.", modifier_label(next)),
            );
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta| {
            let Some(ui) = ui_weak.upgrade() else {
                return false;
            };
            let settings_open = ui.get_settings_open();
            let action = {
                let st = state_ref.borrow();
                settings::match_palette_shortcut(
                    &st.settings,
                    settings_open,
                    text.as_str(),
                    ctrl,
                    alt,
                    shift,
                    meta,
                )
            };
            match action {
                Some(PaletteShortcutAction::ToggleSettings) => {
                    toggle_settings_view(&ui, &rec_flag);
                    true
                }
                Some(PaletteShortcutAction::SelectCatalog(target)) => {
                    if settings_open {
                        stop_shortcut_recording(&ui, Some(&rec_flag));
                        ui.set_settings_feedback("".into());
                        ui.set_settings_open(false);
                        ui.invoke_focus_search();
                    }
                    let mut st = state_ref.borrow_mut();
                    jump_to_catalog(&ui, &mut st, target);
                    true
                }
                None => false,
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        ui.on_toggle_default_effect_controls(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                toggle_default_effect_controls_setting(&ui, &mut st, &hk_ctrl, &tray_ref);
            }
        });
    }
    {
        let state_ref = state.clone();
        ui.on_open_config_file(move || {
            let st = state_ref.borrow();
            open_config_file_in_editor(&st);
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let tray_ref = tray.clone();
        ui.on_reload_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                reload_settings_live(&ui, &mut st, &hk_ctrl, &tray_ref);
            }
        });
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
        let rec_flag = recording_hotkey_flag.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state == HotKeyState::Pressed
                && !rec_flag.load(Ordering::Acquire)
                && event.id == hotkey_id_ref.load(Ordering::Acquire)
            {
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
    let foreground_hook_active = Rc::new(std::cell::Cell::new(false));
    let sync_palette_focus = {
        let state_ref = state.clone();
        let hook_active = foreground_hook_active.clone();
        let rec_flag = recording_hotkey_flag.clone();
        Rc::new(move |ui: &SpotlightWindow| {
            if !state_ref.borrow().is_visible {
                stop_focus_grace_timer();
                return;
            }
            let our_hwnd = extract_window_hwnd(ui);
            win_focus::init_palette_window(our_hwnd);
            let is_our_window = win_focus::is_palette_open_and_active(our_hwnd);

            let (focus_outcome, saw_foreground) = {
                let mut st = state_ref.borrow_mut();
                let outcome = evaluate_palette_focus(&mut st, is_our_window, Instant::now());
                (outcome, st.saw_foreground)
            };
            match focus_outcome {
                FocusOutcome::KeepVisible => {
                    // Once Windows confirms foreground and WinEventHook is active, stop the 20ms grace timer.
                    if saw_foreground && hook_active.get() {
                        stop_focus_grace_timer();
                    }
                }
                FocusOutcome::RetryForeground => {
                    win_focus::force_foreground(our_hwnd);
                }
                FocusOutcome::HideLostFocus => {
                    debug_log::log_event(LogKind::Focus, "FOCUS", "Lost focus -> hide palette");
                    win_focus::set_palette_visible(false);
                    stop_focus_grace_timer();
                    stop_shortcut_recording(&ui, Some(&rec_flag));
                    ui.set_settings_open(false);
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
    let foreground_hook = win_focus::ForegroundHook::install(|| {
        let _ = slint::invoke_from_event_loop(|| {
            SYNC_FOCUS_DISPATCH.with(|slot| {
                if let Some(callback) = slot.borrow_mut().as_mut() {
                    callback();
                }
            });
        });
    });
    foreground_hook_active.set(foreground_hook.is_active());

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let tray_ref = tray.clone();
        let bridge_ref = bridge.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let rec_flag = recording_hotkey_flag.clone();
        tray::set_action_handler(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
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
                    TrayAction::OpenSettings => {
                        let (_, unminimized_premiere_hwnd) =
                            win_focus::query_premiere_window_state();
                        let mut st = state_ref.borrow_mut();
                        st.previous_hwnd = unminimized_premiere_hwnd;
                        stop_shortcut_recording(&ui, Some(&rec_flag));
                        ui.set_settings_feedback("".into());
                        ui.set_settings_open(true);
                        show_palette(&ui, &mut st);
                    }
                    TrayAction::ToggleDefaultEffectControls => {
                        let mut st = state_ref.borrow_mut();
                        toggle_default_effect_controls_setting(
                            &ui,
                            &mut st,
                            &hk_ctrl,
                            &tray_ref,
                        );
                    }
                    TrayAction::OpenConfigFile => {
                        let st = state_ref.borrow();
                        open_config_file_in_editor(&st);
                    }
                    TrayAction::ReloadConfig => {
                        let mut st = state_ref.borrow_mut();
                        reload_settings_live(&ui, &mut st, &hk_ctrl, &tray_ref);
                    }
                    TrayAction::Quit => {
                        if let Some(ref mut t) = *tray_ref.borrow_mut() {
                            t.remove();
                        }
                        std::process::exit(0);
                    }
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let error_toast = error_toast.clone();
        DEADLINE_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                let Some(ui) = ui_weak.upgrade() else {
                    return;
                };
                let mut st = state_ref.borrow_mut();
                st.pending_effects
                    .retain(|_, pending| Instant::now() < pending.deadline);
                if let Some(message) = update_placement(&ui, &mut st, vec![]) {
                    debug_log::log_event(LogKind::Error, "PLACE", message.as_str());
                    error_toast.show(message, st.previous_hwnd);
                }
                if let Some(message) = update_source_open(&ui, &mut st, vec![]) {
                    debug_log::log_event(LogKind::Error, "SOURCE", message.as_str());
                    error_toast.show(message, st.previous_hwnd);
                }
                schedule_action_deadlines(&st);
            }));
        });
    }

    // Keep event loop alive while helper is active in the System Tray
    slint::run_event_loop_until_quit()?;
    Ok(())
}

fn start_focus_grace_timer() {
    FOCUS_TIMER.with(|slot| {
        let mut guard = slot.borrow_mut();
        let timer = guard.get_or_insert_with(Timer::default);
        timer.start(TimerMode::Repeated, Duration::from_millis(20), || {
            debug_log::record_ui_timer_tick();
            SYNC_FOCUS_DISPATCH.with(|dispatch| {
                if let Some(callback) = dispatch.borrow_mut().as_mut() {
                    callback();
                }
            });
        });
    });
}

fn stop_focus_grace_timer() {
    FOCUS_TIMER.with(|slot| {
        if let Some(timer) = slot.borrow().as_ref() {
            timer.stop();
        }
    });
}

fn schedule_action_deadlines(state: &AppState) {
    let earliest = [
        state.pending_placement.as_ref().map(|p| p.deadline),
        state.pending_source_open.as_ref().map(|p| p.deadline),
    ]
    .into_iter()
    .flatten()
    .chain(state.pending_effects.values().map(|p| p.deadline))
    .min();

    DEADLINE_TIMER.with(|slot| {
        if let Some(deadline) = earliest {
            let delay = deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10);
            let mut guard = slot.borrow_mut();
            let timer = guard.get_or_insert_with(Timer::default);
            timer.start(TimerMode::SingleShot, delay, || {
                DEADLINE_DISPATCH.with(|dispatch| {
                    if let Some(callback) = dispatch.borrow_mut().as_mut() {
                        callback();
                    }
                });
            });
        } else if let Some(timer) = slot.borrow().as_ref() {
            timer.stop();
        }
    });
}

thread_local! {
    static BRIDGE_DISPATCH: RefCell<Option<Box<dyn FnMut(BridgeEvent)>>> = const { RefCell::new(None) };
    static HOTKEY_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static SYNC_FOCUS_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static DEADLINE_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static FOCUS_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    static DEADLINE_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
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
        let effect = SearchResult::Effect(Arc::new(EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        }));
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
            matched_items: vec![effect, SearchResult::ProjectItem(Arc::new(item))],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: Some(catalog.clone()),
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings::default(),
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
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
    fn shift_activate_handles_effects_and_project_items() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        let ui = SpotlightWindow::new().unwrap();
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));
        ui.invoke_focus_search();

        let shift_selected = Rc::new(std::cell::Cell::new(0u32));
        let shift_clicked = Rc::new(std::cell::RefCell::new(Vec::new()));
        let activated = Rc::new(std::cell::Cell::new(0u32));
        let clicked = Rc::new(std::cell::Cell::new(0u32));
        {
            let shift_selected = shift_selected.clone();
            ui.on_shift_activate_selected(move || shift_selected.set(shift_selected.get() + 1));
            let shift_clicked = shift_clicked.clone();
            ui.on_shift_activate_index(move |idx| shift_clicked.borrow_mut().push(idx));
            let activated = activated.clone();
            ui.on_activate_selected(move || activated.set(activated.get() + 1));
            let clicked = clicked.clone();
            ui.on_activate_index(move |_| clicked.set(clicked.get() + 1));
        }
        let press = |key: Key| {
            window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        };
        press(Key::Return);
        assert_eq!((shift_selected.get(), activated.get()), (0, 1));
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        press(Key::Return);
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        assert_eq!((shift_selected.get(), activated.get()), (1, 1));

        let effect = SearchResult::Effect(Arc::new(EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        }));
        let item = |id: &str, kind| catalog::ProjectItemEntry {
            item_id: id.into(),
            name: id.into(),
            bin_path: "Footage".into(),
            kind,
            offline: false,
        };
        let catalog = MediaCatalog {
            peer_id: 7,
            project_id: Some("project".into()),
            sequence_id: None,
            items: vec![item("clip", ProjectItemKind::Media), item("bin", ProjectItemKind::Bin)],
        };
        let mut state = AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![
                effect,
                SearchResult::ProjectItem(Arc::new(catalog.items[0].clone())),
                SearchResult::ProjectItem(Arc::new(catalog.items[1].clone())),
            ],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: Some(catalog.clone()),
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings::default(),
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };
        sync_viewport_slice(&ui, &mut state);

        // Shift+Click on a result row invokes shift_activate_index with that row index.
        let row_pos = LogicalPosition::new(120., 78.);
        let button = PointerEventButton::Left;
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::Shift.into() });
        window.dispatch_event(WindowEvent::PointerPressed { position: row_pos, button });
        window.dispatch_event(WindowEvent::PointerReleased { position: row_pos, button });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::Shift.into() });
        assert_eq!(&*shift_clicked.borrow(), &[0]);
        assert_eq!(clicked.get(), 0);

        let bridge = BridgeClients::new();
        let requests = bridge.connect_test_peer(catalog);

        // Shift+Enter/Click on an effect applies it and requests opening Effect Controls on ok=true.
        show_palette(&ui, &mut state);
        shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        assert_eq!(msg["type"], "apply_effect");
        let req_id = msg["requestId"].as_u64().unwrap();
        assert!(state.pending_effects.get(&req_id).unwrap().open_effect_controls);
        assert_eq!(
            complete_effect(
                &mut state,
                &ApplyResult {
                    peer_id: 7,
                    request_id: Some(req_id),
                    ok: false,
                    message: "Select a clip first.".into(),
                }
            ),
            None
        );

        show_palette(&ui, &mut state);
        shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
        let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        let req_id = msg["requestId"].as_u64().unwrap();
        assert_eq!(
            complete_effect(
                &mut state,
                &ApplyResult {
                    peer_id: 7,
                    request_id: Some(req_id),
                    ok: true,
                    message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
                }
            ),
            Some(true)
        );

        // Regular Enter/Click applies without requesting Effect Controls.
        show_palette(&ui, &mut state);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        let req_id = msg["requestId"].as_u64().unwrap();
        assert_eq!(
            complete_effect(
                &mut state,
                &ApplyResult {
                    peer_id: 7,
                    request_id: Some(req_id),
                    ok: true,
                    message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
                }
            ),
            Some(false)
        );

        // Bins are rejected without hiding.
        show_palette(&ui, &mut state);
        let error = shift_activate_at(2, &ui, &mut state, &bridge).unwrap_err();
        assert!(error.contains("Bins"));
        assert!(state.is_visible);
        assert!(requests.try_recv().is_err());

        for success in [true, false] {
            show_palette(&ui, &mut state);
            shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
            assert!(!state.is_visible);
            assert!(requests.try_recv().unwrap().contains("open_in_source_monitor"));
            let request_id = state.pending_source_open.as_ref().unwrap().request_id;
            let error = update_source_open(&ui, &mut state, vec![PlacementResult {
                peer_id: 7,
                request_id,
                ok: success,
                message: if success { "Opened clip" } else { "Too old" }.into(),
            }]);
            assert_eq!(error.as_deref(), if success { None } else { Some("Too old") });
            assert!(state.pending_source_open.is_none());
            assert!(state.pending_placement.is_none());
        }

        shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
        state.pending_source_open.as_mut().unwrap().deadline = Instant::now();
        assert!(update_source_open(&ui, &mut state, vec![]).unwrap().contains("didn't respond"));
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
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings::default(),
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: true,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };
        let effect = Arc::new(EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        });
        let pending = || PendingEffect {
            entry: effect.clone(),
            recipients: vec![7],
            deadline: Instant::now() + ACTION_TIMEOUT,
            open_effect_controls: false,
        };
        let ack = |request_id, ok| ApplyResult {
            peer_id: 7,
            request_id,
            ok,
            message: r#"Applied "Gaussian Blur" to 1 video clip."#.into(),
        };
        state.pending_effects.insert(1, pending());
        assert_eq!(complete_effect(&mut state, &ack(Some(2), true)), None);
        assert_eq!(complete_effect(&mut state, &ack(Some(1), false)), None);
        assert!(state.recent_items.is_empty());
        state.pending_effects.insert(3, pending());
        state.pending_effects.insert(4, pending());
        assert_eq!(complete_effect(&mut state, &ack(None, true)), None);
        assert_eq!(complete_effect(&mut state, &ack(Some(3), true)), Some(false));
        let mut other_peer = ack(None, true);
        other_peer.peer_id = 8;
        assert_eq!(complete_effect(&mut state, &other_peer), None);
        assert_eq!(complete_effect(&mut state, &ack(None, true)), Some(false));
        assert_eq!(complete_effect(&mut state, &ack(None, true)), None);
        let mut both_peers = pending();
        both_peers.recipients.push(8);
        state.pending_effects.insert(5, both_peers);
        assert_eq!(complete_effect(&mut state, &ack(Some(5), false)), None);
        let mut success = ack(Some(5), true);
        success.peer_id = 8;
        assert_eq!(complete_effect(&mut state, &success), Some(false));
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
            item: SearchResult::ProjectItem(Arc::new(item.clone())), project_id: Some("project".into()),
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

    #[test]
    fn f12_toggles_debug_log_window() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let _ = slint::platform::set_platform(Box::new(TestPlatform(adapter.clone())));
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        let ui = SpotlightWindow::new().unwrap();
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));

        let toggled = Rc::new(std::cell::Cell::new(0u32));
        {
            let toggled = toggled.clone();
            ui.on_toggle_debug_log(move || toggled.set(toggled.get() + 1));
        }

        let press_f12 = || {
            window.dispatch_event(WindowEvent::KeyPressed { text: Key::F12.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: Key::F12.into() });
        };

        // Works both when search-input has focus and when FocusScope handles it.
        ui.invoke_focus_search();
        press_f12();
        assert_eq!(toggled.get(), 1);

        let footer_pos = LogicalPosition::new(300., 390.);
        window.dispatch_event(WindowEvent::PointerPressed {
            position: footer_pos,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: footer_pos,
            button: PointerEventButton::Left,
        });
        press_f12();
        assert_eq!(toggled.get(), 2);

        // DebugLogController records logs and toggles open/closed cleanly.
        debug_log::log_event(LogKind::Info, "TEST", "F12 debug window check");
        let ctrl = Rc::new(DebugLogController::default());
        ctrl.toggle(0);
        assert!(ctrl.is_open());
        ctrl.toggle(0);
        assert!(!ctrl.is_open());
    }

    #[test]
    fn moving_selection_inside_visible_window_reuses_results_model() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let _ = slint::platform::set_platform(Box::new(TestPlatform(adapter)));
        let ui = SpotlightWindow::new().unwrap();
        let mut store = CatalogStore::new_with_defaults();
        store.set_project_items(
            (1..=12)
                .map(|i| catalog::ProjectItemEntry {
                    item_id: i.to_string(),
                    name: format!("Clip {i:02}"),
                    bin_path: "Footage".into(),
                    kind: ProjectItemKind::Media,
                    offline: false,
                })
                .collect(),
        );
        let mut state = AppState {
            catalog: store,
            matched_items: vec![],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: None,
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings::default(),
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };
        use slint::Model;
        let model_ptr = |ui: &SpotlightWindow| -> *const VecModel<EffectItemUI> {
            ui.get_results()
                .as_any()
                .downcast_ref::<VecModel<EffectItemUI>>()
                .unwrap() as *const VecModel<EffectItemUI>
        };
        ui.set_active_catalog("project".into());
        refresh_results_ui(&ui, &mut state);
        let initial_ptr = model_ptr(&ui);

        state.selected_abs_index = 2;
        sync_viewport_slice(&ui, &mut state);
        assert_eq!(model_ptr(&ui), initial_ptr);
        assert_eq!(ui.get_selected_index(), 2);

        state.selected_abs_index = VISIBLE_ROWS;
        sync_viewport_slice(&ui, &mut state);
        assert_ne!(model_ptr(&ui), initial_ptr);
        assert_eq!(state.scroll_offset, 1);
        assert_eq!(ui.get_selected_index(), (VISIBLE_ROWS - 1) as i32);
    }

    #[test]
    fn bin_drill_down_enters_and_steps_out_without_hiding() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let bridge = BridgeClients::new();
        let item = |id: &str, name: &str, bin_path: &str, kind: ProjectItemKind| ProjectItemEntry {
            item_id: id.into(),
            name: name.into(),
            bin_path: bin_path.into(),
            kind,
            offline: false,
        };
        let items = vec![
            item("b1", "Footage", "Project root", ProjectItemKind::Bin),
            item("b2", "B-Roll", "Footage", ProjectItemKind::Bin),
            item("c1", "Drone.mp4", "Footage / B-Roll", ProjectItemKind::Media),
            item("c2", "Music.wav", "Audio", ProjectItemKind::Media),
        ];
        let catalog = MediaCatalog {
            peer_id: 7,
            project_id: Some("proj".into()),
            sequence_id: Some("seq".into()),
            items: items.clone(),
        };
        let requests = bridge.connect_test_peer(catalog.clone());
        let mut store = CatalogStore::new_with_defaults();
        store.set_project_items(items);
        let mut state = AppState {
            catalog: store,
            matched_items: vec![],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: Some(catalog),
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings::default(),
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };

        show_palette(&ui, &mut state);
        ui.set_search_text("Footage".into());
        refresh_results_ui(&ui, &mut state);
        let footage_idx = state
            .matched_items
            .iter()
            .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b1"))
            .unwrap();

        apply_entry_at(footage_idx, &ui, &mut state, &bridge).unwrap();
        assert!(state.is_visible);
        assert!(requests.try_recv().is_err());
        assert!(state.pending_placement.is_none());
        assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
        assert_eq!(ui.get_active_bin().as_str(), "Footage");
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        assert!(ui.get_search_text().is_empty());
        assert_eq!(state.matched_items.len(), 2);

        let broll_idx = state
            .matched_items
            .iter()
            .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b2"))
            .unwrap();
        apply_entry_at(broll_idx, &ui, &mut state, &bridge).unwrap();
        assert_eq!(state.active_bin_path.as_deref(), Some("Footage / B-Roll"));
        assert_eq!(ui.get_active_bin().as_str(), "Footage / B-Roll");
        assert_eq!(state.matched_items.len(), 1);

        step_out_bin(&ui, &mut state);
        assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
        assert_eq!(ui.get_active_bin().as_str(), "Footage");
        assert_eq!(state.matched_items.len(), 2);

        step_out_bin(&ui, &mut state);
        assert!(state.active_bin_path.is_none());
        assert!(ui.get_active_bin().is_empty());
        assert_eq!(state.matched_items.len(), 4);

        let footage_idx = state
            .matched_items
            .iter()
            .position(|result| matches!(result, SearchResult::ProjectItem(e) if e.item_id == "b1"))
            .unwrap();
        apply_entry_at(footage_idx, &ui, &mut state, &bridge).unwrap();
        assert_eq!(state.active_bin_path.as_deref(), Some("Footage"));
        center_and_show(&ui, &mut state, &bridge);
        assert!(state.active_bin_path.is_none());
        assert!(ui.get_active_bin().is_empty());
    }

    #[test]
    fn backspace_on_empty_search_triggers_step_out_bin_in_slint() {
        use std::cell::Cell;

        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));

        let step_outs = Rc::new(Cell::new(0usize));
        let step_outs_cb = step_outs.clone();
        ui.on_step_out_bin(move || {
            step_outs_cb.set(step_outs_cb.get() + 1);
        });

        let press_backspace = || {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: Key::Backspace.into(),
            });
            window.dispatch_event(WindowEvent::KeyReleased {
                text: Key::Backspace.into(),
            });
        };

        ui.set_active_bin("Footage".into());
        ui.set_search_text("".into());
        ui.invoke_focus_search();
        for ch in ["a", "b"] {
            window.dispatch_event(WindowEvent::KeyPressed { text: ch.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: ch.into() });
        }
        assert_eq!(ui.get_search_text().as_str(), "ab");

        press_backspace();
        assert_eq!(step_outs.get(), 0);
        assert_eq!(ui.get_search_text().as_str(), "a");

        press_backspace();
        assert_eq!(step_outs.get(), 0);
        assert_eq!(ui.get_search_text().as_str(), "");

        press_backspace();
        assert_eq!(step_outs.get(), 1);
    }

    #[test]
    fn selected_project_folder_renders_beside_name_and_separated() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));

        let render = |name: &str, folder: &str| -> Vec<Rgb8Pixel> {
            let item = EffectItemUI {
                name: name.into(),
                category: "Sequence".into(),
                folder: folder.into(),
                badge_bg: Color::from_rgb_u8(0x36, 0x26, 0x5c),
                badge_fg: Color::from_rgb_u8(0xdd, 0xd6, 0xfe),
            };
            ui.set_results(ModelRc::new(VecModel::from(vec![item])));
            ui.set_selected_index(0);
            let width = WINDOW_WIDTH as usize;
            let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
            window.request_redraw();
            assert!(adapter.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, width);
            }));
            pixels
        };

        let width = WINDOW_WIDTH as usize;
        let without_folder = render("00_BACK UP", "");
        let with_folder = render("00_BACK UP", "04_Nests / 041_Main");

        let diff_xs: Vec<usize> = (62..96)
            .flat_map(|y| {
                (18..500).filter_map({
                    let without_folder = &without_folder;
                    let with_folder = &with_folder;
                    move |x| (without_folder[y * width + x] != with_folder[y * width + x]).then_some(x)
                })
            })
            .collect();
        assert!(!diff_xs.is_empty());
        // Name itself shouldn't shift vertically; divider and folder pill sit strictly to its right.
        let min_diff_x = *diff_xs.iter().min().unwrap();
        assert!(min_diff_x > 80, "expected divider/folder beside name, got min x={min_diff_x}");

        // Even with long name + folder, neither overlaps the right-side category pill (x >= 504).
        let long_base = render("Very_Long_Sequence_Name_That_Must_Elide_Cleanly_Before_The_Divider", "");
        let long_with_folder = render(
            "Very_Long_Sequence_Name_That_Must_Elide_Cleanly_Before_The_Divider",
            "01_Project / 04_Nests / 041_Main / Exports / Final_Master",
        );
        let leaked_into_badge = (62..96).any(|y| {
            (504..WINDOW_WIDTH as usize - 10)
                .any(|x| long_base[y * width + x] != long_with_folder[y * width + x])
        });
        assert!(!leaked_into_badge);
    }

    #[test]
    fn default_effect_controls_setting_swaps_enter_and_shift_enter_for_effects() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let effect = SearchResult::Effect(Arc::new(EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        }));
        let clip = catalog::ProjectItemEntry {
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
            items: vec![clip.clone()],
        };
        let bridge = BridgeClients::new();
        let requests = bridge.connect_test_peer(catalog.clone());
        let mut state = AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![effect, SearchResult::ProjectItem(Arc::new(clip))],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: Some(catalog),
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: None,
            settings: AppSettings {
                hotkey: "Alt+Space".into(),
                open_effect_controls_by_default: true,
                ..AppSettings::default()
            },
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        };

        show_palette(&ui, &mut state);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        let msg: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        let req1 = msg["requestId"].as_u64().unwrap();
        assert!(state.pending_effects.get(&req1).unwrap().open_effect_controls);

        show_palette(&ui, &mut state);
        shift_activate_at(0, &ui, &mut state, &bridge).unwrap();
        let msg2: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        let req2 = msg2["requestId"].as_u64().unwrap();
        assert!(!state.pending_effects.get(&req2).unwrap().open_effect_controls);

        // Project items still open in Source Monitor on Shift+Enter and place on Enter.
        show_palette(&ui, &mut state);
        shift_activate_at(1, &ui, &mut state, &bridge).unwrap();
        assert!(requests.try_recv().unwrap().contains("open_in_source_monitor"));
    }

    #[test]
    fn settings_gear_toggles_in_palette_settings_view_and_esc_returns_to_search() {
        use std::cell::Cell;

        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));

        let closed = Rc::new(Cell::new(0usize));
        let closed_cb = closed.clone();
        ui.on_close_requested(move || closed_cb.set(closed_cb.get() + 1));

        let ui_weak = ui.as_weak();
        ui.on_toggle_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_settings_open(!ui.get_settings_open());
            }
        });

        let captured = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
        let captured_cb = captured.clone();
        ui.on_capture_shortcut(move |target, text, ctrl, alt, shift, meta| {
            if let Ok(Some((_, label))) =
                settings::hotkey_from_slint_event(text.as_str(), ctrl, alt, shift, meta)
            {
                captured_cb.borrow_mut().push((target.to_string(), label));
            }
        });

        // Click the lower-right gear icon (around x=594, y=389 inside 620x407 footer).
        let gear_pos = LogicalPosition::new(594., 389.);
        window.dispatch_event(WindowEvent::PointerPressed {
            position: gear_pos,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: gear_pos,
            button: PointerEventButton::Left,
        });
        assert!(ui.get_settings_open());

        // Clicking Launch Hotkey shortcut box enters recording mode and Esc cancels recording without leaving Settings.
        let recorder_pos = LogicalPosition::new(490., 104.);
        window.dispatch_event(WindowEvent::PointerPressed {
            position: recorder_pos,
            button: PointerEventButton::Left,
        });
        window.dispatch_event(WindowEvent::PointerReleased {
            position: recorder_pos,
            button: PointerEventButton::Left,
        });
        assert!(ui.get_recording_hotkey());
        assert_eq!(ui.get_recording_target().as_str(), "launch");

        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Alt.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: " ".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: " ".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Alt.into(),
        });
        assert_eq!(
            &*captured.borrow(),
            &[("launch".to_string(), "Alt+Space".to_string())]
        );

        // Esc exits recording first, keeping Settings open; second Esc exits Settings without closing Spotlight.
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Escape.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Escape.into(),
        });
        assert!(!ui.get_recording_hotkey());
        assert!(ui.get_settings_open());
        assert_eq!(closed.get(), 0);

        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Escape.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Escape.into(),
        });
        assert!(!ui.get_settings_open());
        assert_eq!(closed.get(), 0);
    }

    #[test]
    fn configurable_modifier_settings_hotkey_and_catalog_shortcuts_work() {
        use std::cell::Cell;

        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        let window = ui.window();
        window.dispatch_event(WindowEvent::WindowActiveChanged(true));

        let clip = catalog::ProjectItemEntry {
            item_id: "clip".into(),
            name: "Clip".into(),
            bin_path: "01_Footage".into(),
            kind: ProjectItemKind::Media,
            offline: false,
        };
        let state = Rc::new(RefCell::new(AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![SearchResult::ProjectItem(Arc::new(clip))],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: None,
            pending_placement: None,
            pending_source_open: None,
            active_bin_path: Some("01_Footage".into()),
            settings: AppSettings {
                settings_hotkey: "Ctrl+,".into(),
                catalog_effects_hotkey: "Ctrl+2".into(),
                ..AppSettings::default()
            },
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: true,
            shown_at: Some(Instant::now()),
            saw_foreground: true,
            previous_hwnd: 0,
        }));
        {
            let mut st = state.borrow_mut();
            ui.set_active_catalog("project".into());
            ui.set_active_bin("01_Footage".into());
            ui.set_settings_hotkey_label(st.settings.settings_hotkey.clone().into());
            ui.set_catalog_effects_hotkey_label(st.settings.catalog_effects_hotkey.clone().into());
            refresh_results_ui(&ui, &mut st);
        }

        let primary_calls = Rc::new(Cell::new(0usize));
        let alternate_calls = Rc::new(Cell::new(0usize));
        let primary_cb = primary_calls.clone();
        ui.on_activate_selected(move || primary_cb.set(primary_cb.get() + 1));
        let alt_cb = alternate_calls.clone();
        ui.on_shift_activate_selected(move || alt_cb.set(alt_cb.get() + 1));

        let ui_weak = ui.as_weak();
        ui.on_toggle_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_settings_open(!ui.get_settings_open());
            }
        });

        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta| {
            let Some(ui) = ui_weak.upgrade() else {
                return false;
            };
            let settings_open = ui.get_settings_open();
            let action = {
                let st = state_ref.borrow();
                settings::match_palette_shortcut(
                    &st.settings,
                    settings_open,
                    text.as_str(),
                    ctrl,
                    alt,
                    shift,
                    meta,
                )
            };
            match action {
                Some(PaletteShortcutAction::ToggleSettings) => {
                    ui.set_settings_open(!settings_open);
                    true
                }
                Some(PaletteShortcutAction::SelectCatalog(target)) => {
                    let mut st = state_ref.borrow_mut();
                    ui.set_active_catalog(target.into());
                    if target != "project" {
                        clear_bin_scope(&ui, &mut st);
                    }
                    true
                }
                None => false,
            }
        });

        // Switch alternate action modifier to Ctrl: Ctrl+Enter fires shift-activate, Shift+Enter fires primary.
        ui.set_alternate_modifier("ctrl".into());
        ui.invoke_focus_search();
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Return.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Return.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert_eq!(alternate_calls.get(), 1);
        assert_eq!(primary_calls.get(), 0);

        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Shift.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Return.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Return.into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Shift.into(),
        });
        assert_eq!(alternate_calls.get(), 1);
        assert_eq!(primary_calls.get(), 1);

        // Ctrl+, toggles Settings open and closed even while search input had focus.
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: ",".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: ",".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(ui.get_settings_open());

        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: ",".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: ",".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert!(!ui.get_settings_open());

        // Configured direct catalog shortcut Ctrl+2 jumps straight to Effects and clears Project bin scope.
        ui.invoke_focus_search();
        window.dispatch_event(WindowEvent::KeyPressed {
            text: Key::Control.into(),
        });
        window.dispatch_event(WindowEvent::KeyPressed {
            text: "2".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: "2".into(),
        });
        window.dispatch_event(WindowEvent::KeyReleased {
            text: Key::Control.into(),
        });
        assert_eq!(ui.get_active_catalog().as_str(), "effects");
        assert!(state.borrow().active_bin_path.is_none());
        assert_eq!(ui.get_search_text().as_str(), "");
    }
}
