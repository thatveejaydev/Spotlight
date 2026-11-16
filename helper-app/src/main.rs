#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action_hotkeys;
mod actions;
mod audio_preview;
mod bridge;
mod catalog;
mod cursor_lock;
mod debug_log;
mod error_toast;
mod installer;
mod presets;
mod safe_file;
mod settings;
mod tray;
mod win_focus;

use actions::{
    markers::{MarkerDialog, MarkerEvent, MarkerPicker},
    Action, ActionResult, PendingAction,
};
use audio_preview::AudioPreview;
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
use presets::PresetHandoff;
use settings::{AlternateModifier, AppSettings, PaletteShortcutAction, PaletteTheme};
use slint::{
    Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel,
};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::Path,
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
    pending_action: Option<PendingAction>,
    marker_picker: Option<MarkerPicker>,
    marker_dialog: Option<MarkerDialog>,
    audio_preview: AudioPreview,
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
        "preset" => (
            Color::from_argb_u8(68, 248, 78, 66),
            Color::from_rgb_u8(255, 174, 162),
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
            SearchResult::Action(_) | SearchResult::Marker(_) => None,
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
    rebuild_results_ui(ui, state);
    sync_audio_selection(ui, state);
}

fn sync_audio_selection(ui: &SpotlightWindow, state: &mut AppState) {
    let item = if state.is_visible && !ui.get_settings_open() {
        match state.matched_items.get(state.selected_abs_index) {
            Some(SearchResult::ProjectItem(item)) => Some(item.as_ref()),
            _ => None,
        }
    } else {
        None
    };
    state.audio_preview.select(
        ui,
        state.media_catalog.as_ref(),
        item,
        state.settings.auto_preview_audio,
    );
}

fn rebuild_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    let query = ui.get_search_text();
    let catalog = ui.get_active_catalog();
    let show_history =
        state.active_bin_path.is_none() && catalog.as_str() == "all" && query.trim().is_empty();
    ui.set_show_recent_items(show_history);
    let started = Instant::now();
    state.matched_items = if let Some(picker) = &state.marker_picker {
        state.catalog.search_markers(query.as_str(), picker.markers())
    } else if show_history {
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
            if let Some(picker) = &state.marker_picker {
                picker.markers().len()
            } else if catalog.as_str() == "actions" {
                Action::ALL.len()
            } else if catalog.as_str() == "all" && !show_history {
                effects_count + media_count + Action::ALL.len()
            } else {
                effects_count + media_count
            },
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
    rebuild_results_ui(ui, state);
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
    sync_audio_selection(ui, state);
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
            let position;
            let (name, category, folder, colors) = match entry {
                SearchResult::Marker(marker) => {
                    position = marker.position();
                    (marker.title(), position.as_str(), marker.sequence_name.as_str(), badge_colors("action"))
                }
                SearchResult::Action(action) => (
                    action.name(),
                    "Action",
                    action.description(),
                    badge_colors("action"),
                ),
                SearchResult::Effect(entry) => (
                    entry.name.as_str(),
                    entry.category_short(),
                    if entry.is_preset() {
                        entry.folder.as_str()
                    } else {
                        ""
                    },
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
                folder: if matches!(entry, SearchResult::Marker(_)) {
                    folder.into()
                } else {
                    folder.replace(" / ", " › ").into()
                },
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
    clear_marker_picker(ui, state);
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
    win_focus::set_palette_hwnd(our_hwnd);
    if our_hwnd != our_hwnd_before {
        win_focus::center_palette(our_hwnd, state.previous_hwnd);
    }
    update_palette_caption(ui, our_hwnd);
    win_focus::ensure_palette_topmost(our_hwnd);
    win_focus::force_foreground(our_hwnd);
    debug_log::log_event(
        LogKind::Focus,
        "SHOW",
        format!(
            "palette={our_hwnd:#x} fg_after_force={:#x} prev={:#x}",
            win_focus::get_foreground_hwnd(),
            state.previous_hwnd
        ),
    );
    win_focus::set_palette_visible(true);
    start_focus_grace_timer();
    if !ui.get_settings_open() {
        ui.invoke_focus_search();
    }
    sync_audio_selection(ui, state);
}

fn update_palette_caption(ui: &SpotlightWindow, hwnd: isize) {
    let color = ui.get_caption_color();
    win_focus::set_palette_caption_color(hwnd, [color.red(), color.green(), color.blue()]);
}

fn apply_palette_theme(ui: &SpotlightWindow, theme: PaletteTheme) {
    ui.set_theme_name(theme.label().into());
    update_palette_caption(ui, extract_window_hwnd(ui));
}

fn change_palette_theme(ui: &SpotlightWindow, settings: &mut AppSettings, label: &str, path: &Path) {
    let Some(theme) = PaletteTheme::from_label(label) else {
        return;
    };
    if theme == settings.theme {
        return;
    }

    let updated = AppSettings {
        theme,
        ..settings.clone()
    };
    match settings::save_settings_to(path, &updated) {
        Ok(()) => {
            *settings = updated;
            apply_palette_theme(ui, theme);
            ui.set_settings_feedback("".into());
            ui.set_settings_feedback_error(false);
        }
        Err(error) => {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(error.into());
        }
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
    clear_marker_picker(ui, state);
    state.audio_preview.cancel(ui);
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

fn clear_marker_picker(ui: &SpotlightWindow, state: &mut AppState) -> Option<MarkerPicker> {
    let picker = state.marker_picker.take()?;
    ui.set_marker_picker(false);
    ui.set_marker_status("".into());
    ui.set_marker_context("".into());
    ui.set_search_text("".into());
    schedule_action_deadlines(state);
    Some(picker)
}

fn escape_palette(ui: &SpotlightWindow, state: &mut AppState) {
    if let Some(picker) = clear_marker_picker(ui, state) {
        ui.set_search_text(picker.previous_query);
        refresh_marker_results(ui, state);
        ui.invoke_focus_search();
    } else {
        hide_and_restore_focus(ui, state);
    }
}

fn refresh_marker_results(ui: &SpotlightWindow, state: &mut AppState) {
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
}

fn receive_marker(
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    event: MarkerEvent,
    toast: &Rc<ErrorToast>,
) {
    if let Some(picker) = state.marker_picker.as_mut().filter(|picker| picker.accepts(&event)) {
        let sought = picker.receive(event);
        ui.set_marker_context(picker.context_label.as_str().into());
        if sought {
            hide_and_restore_focus(ui, state);
        } else {
            ui.set_marker_status(picker.status().into());
            refresh_marker_results(ui, state);
        }
    } else if let Some(dialog) = state.marker_dialog.as_mut().filter(|dialog| dialog.accepts(&event)) {
        if let Err(message) = dialog.receive(bridge, event) {
            let message = dialog.failure(message);
            state.marker_dialog = None;
            toast.show(message.into(), state.previous_hwnd);
        }
    }
    schedule_action_deadlines(state);
}

fn poll_markers(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients, toast: &Rc<ErrorToast>) {
    if let Some(picker) = &mut state.marker_picker {
        if picker.expire() {
            ui.set_marker_status(picker.status().into());
        }
    }
    if let Some(dialog) = &mut state.marker_dialog {
        match dialog.tick(bridge) {
            Ok(false) => {}
            Ok(true) => state.marker_dialog = None,
            Err(message) => {
                let message = dialog.failure(message);
                state.marker_dialog = None;
                toast.show(message.into(), state.previous_hwnd);
            }
        }
    }
}

fn finish_action(
    ui: &SpotlightWindow,
    state: &mut AppState,
    result: Option<ActionResult>,
    toast: &Rc<ErrorToast>,
) {
    let Some(outcome) = state.pending_action.as_ref().and_then(|pending| pending.outcome(result)) else {
        return;
    };
    state.pending_action = None;
    match outcome {
        Ok(message) => {
            debug_log::log_event(LogKind::Success, "ACTION", &message);
            ui.set_status_message(message.into());
        }
        Err(message) => {
            debug_log::log_event(LogKind::Error, "ACTION", &message);
            toast.show(message.into(), state.previous_hwnd);
        }
    }
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
) -> Result<Option<PresetHandoff>, SharedString> {
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
) -> Result<Option<PresetHandoff>, SharedString> {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return Ok(None);
    };
    if let SearchResult::Effect(entry) = &chosen {
        if entry.is_preset() {
            let reveal = PresetHandoff::new(state.previous_hwnd, entry.clone())?;
            hide_palette(ui, state);
            restore_premiere_focus(state.previous_hwnd);
            return Ok(Some(reveal));
        }
    }
    if state.pending_placement.is_some() {
        ui.set_status_message("A media placement is already in progress.".into());
        return Ok(None);
    }
    match chosen {
        SearchResult::Marker(marker) => {
            if let Some(picker) = &mut state.marker_picker {
                picker.seek(bridge, marker);
                ui.set_marker_status(picker.status().into());
                schedule_action_deadlines(state);
            }
            return Ok(None);
        }
        SearchResult::Action(action) => {
            return activate_action(action, ui, state, bridge).map(|()| None);
        }
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
                return Ok(None);
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
    Ok(None)
}

fn activate_action(
    action: Action,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<(), SharedString> {
    if state.pending_placement.is_some() {
        return Err("A media placement is already in progress.".into());
    }
    match action {
        Action::GoToMarker => {
            if state.marker_picker.is_some() { return Ok(()); }
            if !state.is_visible {
                center_and_show(ui, state, bridge);
                ui.set_active_catalog("actions".into());
            }
            let picker = MarkerPicker::open(bridge, ui.get_search_text());
            ui.set_marker_picker(true);
            ui.set_marker_status(picker.status().into());
            ui.set_search_text("".into());
            state.marker_picker = Some(picker);
            refresh_marker_results(ui, state);
            ui.invoke_focus_search();
        }
        Action::AddMarker => {
            if state.marker_dialog.is_some() {
                return Err("Add Marker is already in progress.".into());
            }
            state.marker_dialog = Some(MarkerDialog::new(state.previous_hwnd, bridge)?);
        }
        Action::LockHorizontalAxis => {
            if let Some(shortcut) = state
                .settings
                .action_hotkeys
                .get(action.id())
                .filter(|spec| !spec.trim().is_empty())
            {
                ui.set_status_message(
                    format!("Hold {shortcut} in Premiere Pro to lock the horizontal axis.").into(),
                );
            } else {
                ui.set_settings_tab_index(1);
                ui.set_settings_open(true);
                ui.set_settings_feedback_error(false);
                ui.set_settings_feedback(
                    "Assign a shortcut to Lock Horizontal Axis, then hold it while dragging in Premiere."
                        .into(),
                );
            }
        }
        _ => {
            if state.pending_action.is_some() {
                return Err("A Premiere action is already in progress.".into());
            }
            let (request_id, peer_id) = bridge.run_action(action).map_err(SharedString::from)?;
            state.pending_action = Some(PendingAction {
                request_id,
                peer_id,
                deadline: Instant::now() + ACTION_TIMEOUT,
            });
        }
    }
    if action != Action::GoToMarker && action != Action::LockHorizontalAxis && state.is_visible {
        hide_and_restore_focus(ui, state);
    }
    schedule_action_deadlines(state);
    Ok(())
}

fn dispatch_action_hotkey(
    action: Action,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    toast: &Rc<ErrorToast>,
    foreground: Option<isize>,
) {
    if ui.get_settings_open() || ui.get_recording_hotkey() {
        return;
    }
    if let Some(hwnd) = foreground {
        if win_focus::get_foreground_hwnd() != hwnd || !win_focus::is_premiere_action_window(hwnd) {
            return;
        }
        state.previous_hwnd = hwnd;
    } else if !state.is_visible || !win_focus::is_premiere_action_window(state.previous_hwnd) {
        return;
    }
    if let Err(message) = activate_action(action, ui, state, bridge) {
        toast.show(message, state.previous_hwnd);
    }
}

fn shift_activate_at(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<Option<PresetHandoff>, SharedString> {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return Ok(None);
    };
    match chosen {
        SearchResult::Action(_) | SearchResult::Marker(_) => {
            apply_entry_with_options(abs_index, ui, state, bridge, false)
        }
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
            Ok(None)
        }
    }
}

fn finish_activation(
    result: Result<Option<PresetHandoff>, SharedString>,
    toast: &Rc<ErrorToast>,
    premiere_hwnd: isize,
) {
    match result {
        Ok(Some(handoff)) => handoff.start(toast),
        Ok(None) => {}
        Err(message) => toast.show(message, premiere_hwnd),
    }
}

#[cfg(windows)]
struct InstanceMutexGuard(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for InstanceMutexGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }
}

#[cfg(windows)]
fn acquire_instance_mutex() -> Option<InstanceMutexGuard> {
    let name: Vec<u16> = "PremiereSpotlightInstanceMutex\0".encode_utf16().collect();
    let handle = unsafe {
        windows_sys::Win32::System::Threading::CreateMutexW(
            std::ptr::null(),
            0,
            name.as_ptr(),
        )
    };
    if handle.is_null() {
        return None;
    }
    let already_exists = unsafe {
        windows_sys::Win32::Foundation::GetLastError()
            == windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS
    };
    if already_exists {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        None
    } else {
        Some(InstanceMutexGuard(handle))
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has_flag = |flag: &str| args.iter().any(|arg| arg == flag);

    if has_flag("--install") {
        installer::install_for_setup(!has_flag("--no-startup"));
        return Ok(());
    }
    if has_flag("--uninstall") {
        installer::uninstall_for_setup(has_flag("--purge-config"));
        return Ok(());
    }

    // 1. Auto-install / enable the UXP bridge plugin in Premiere Pro and clean up legacy Startup entries
    installer::ensure_installed();

    // 2. Spotlight only runs while Adobe Premiere Pro is open
    if !win_focus::is_premiere_running() {
        return Ok(());
    }

    // 3. Single-instance check: if Premiere Spotlight is already running in the System Tray, wake it (unless --watch) and exit
    if bridge::check_or_wake_existing_instance(!has_flag("--watch")) {
        return Ok(());
    }

    #[cfg(windows)]
    let Some(_instance_mutex) = acquire_instance_mutex() else {
        return Ok(());
    };

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
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    let error_toast = Rc::new(ErrorToast::default());
    let debug_log_ctrl = Rc::new(DebugLogController::default());
    win_focus::init_palette_window(extract_window_hwnd(&ui));
    debug_log::log_event(LogKind::Info, "INIT", "Premiere Spotlight helper started");

    let (mut initial_settings, mut action_hotkey_errors, settings_file_warning) =
        settings::load_settings_with_feedback();
    action_hotkey_errors.extend(settings::normalize_action_hotkeys(&mut initial_settings));
    for message in action_hotkey_errors {
        debug_log::log_event(LogKind::Error, "HOTKEY", &message);
    }
    if let Some(warning) = settings_file_warning {
        let toast = error_toast.clone();
        // Give Premiere a moment to own the foreground so the toast lands on its monitor.
        slint::Timer::single_shot(Duration::from_secs(2), move || {
            let foreground = win_focus::get_foreground_hwnd();
            let anchor = if win_focus::is_premiere_active_and_unminimized(foreground) {
                foreground
            } else {
                0
            };
            toast.show(warning.into(), anchor);
        });
    }

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
        pending_action: None,
        marker_picker: None,
        marker_dialog: None,
        audio_preview: AudioPreview::default(),
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

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        PRESET_CATALOG_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |presets| {
                if let Some(ui) = ui_weak.upgrade() {
                    let mut st = state_ref.borrow_mut();
                    st.catalog.set_presets(presets);
                    refresh_catalog_ui(&ui, &mut st, true);
                }
            }));
        });
        presets::watch_library(|presets| {
            slint::invoke_from_event_loop(move || {
                PRESET_CATALOG_DISPATCH.with(|slot| {
                    if let Some(callback) = slot.borrow_mut().as_mut() {
                        callback(presets);
                    }
                });
            })
            .is_ok()
        });
    }

    // Start localhost WebSocket bridge on 127.0.0.1:3847
    let bridge = BridgeClients::new();
    {
        let ui = ui.as_weak();
        let state_ref = Rc::downgrade(&state);
        let bridge = bridge.clone();
        state.borrow().audio_preview.set_poll_callback(move || {
            if let (Some(ui), Some(state)) = (ui.upgrade(), state_ref.upgrade()) {
                let st = &mut *state.borrow_mut();
                st.audio_preview.poll(&ui, &bridge, st.media_catalog.as_ref());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
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
                    }
                    BridgeEvent::ClientDisconnected => {
                        ui.set_bridge_connected(false);
                        ui.set_status_message(SharedString::from(
                            "Premiere disconnected · Standalone catalog active",
                        ));
                    }
                    BridgeEvent::CatalogReceived(items) => {
                        let hotkey = state_ref.borrow().settings.hotkey.clone();
                        ui.set_bridge_connected(true);
                        ui.set_status_message(SharedString::from(format!(
                            "Premiere synced ({} effects) · {hotkey}",
                            items.len()
                        )));
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
                        if let (Some(picker), Some(catalog)) = (&mut st.marker_picker, &catalog) {
                            if picker.refresh_if_project_changed(&bridge_ref, catalog) {
                                ui.set_marker_context("".into());
                                ui.set_marker_status(picker.status().into());
                                schedule_action_deadlines(&st);
                            }
                        }
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
                    BridgeEvent::ActionResult(result) => {
                        let mut st = state_ref.borrow_mut();
                        finish_action(&ui, &mut st, Some(result), &error_toast);
                        schedule_action_deadlines(&st);
                    }
                    BridgeEvent::MarkerResult(event) => {
                        receive_marker(&ui, &mut state_ref.borrow_mut(), &bridge_ref, event, &error_toast);
                    }
                    BridgeEvent::AudioPreviewResult(result) => {
                        state_ref.borrow_mut().audio_preview.resolved(&ui, result);
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
                st.audio_preview.defer_autoplay();
            }
        });
    }

    // Catalog tabs and the Tab key share the catalog switch.
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_catalog_changed(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                clear_marker_picker(&ui, &mut st);
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
                sync_audio_selection(&ui, &mut st);
            }
        });
    }

    // Enter activates the selected result.
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let error_toast = error_toast.clone();
        ui.on_activate_selected(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                let abs_idx = st.selected_abs_index;
                let result = apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref);
                finish_activation(result, &error_toast, st.previous_hwnd);
            }
        });
    }

    // Mouse click activates the row.
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
                sync_viewport_slice(&ui, &mut st);
                sync_audio_selection(&ui, &mut st);
                let result = apply_entry_at(abs_idx, &ui, &mut st, &bridge_ref);
                finish_activation(result, &error_toast, st.previous_hwnd);
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
                let result = shift_activate_at(abs_idx, &ui, &mut st, &bridge_ref);
                finish_activation(result, &error_toast, st.previous_hwnd);
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
                sync_audio_selection(&ui, &mut st);
                let result = shift_activate_at(abs_idx, &ui, &mut st, &bridge_ref);
                finish_activation(result, &error_toast, st.previous_hwnd);
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

    // Escape leaves a nested marker picker or closes the palette.
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_close_requested(move || {
            if let Some(ui) = ui_weak.upgrade() {
                escape_palette(&ui, &mut state_ref.borrow_mut());
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
        hook_active: bool,
    }

    impl HotkeyController {
        fn new(initial_spec: &str, hook_active: bool) -> (Self, Option<String>) {
            let manager =
                GlobalHotKeyManager::new().expect("Failed to initialize GlobalHotKeyManager");
            let mut ctrl = Self {
                manager,
                registered: None,
                active_id: Arc::new(AtomicU32::new(NO_HOTKEY_ID)),
                available: false,
                hook_active,
            };
            let err = match settings::parse_hotkey_spec(initial_spec) {
                Ok((hotkey, label)) => match ctrl.apply(hotkey, label.clone()) {
                    Ok(()) => None,
                    Err(msg) => {
                        debug_log::log_event(LogKind::Error, "HOTKEY", &msg);
                        Some(msg)
                    }
                },
                Err(msg) => Some(msg),
            };
            (ctrl, err)
        }

        fn unregister(&mut self) {
            if let Some((old_hk, _)) = self.registered.take() {
                if self.available {
                    let _ = self.manager.unregister(old_hk);
                }
            }
            self.active_id.store(NO_HOTKEY_ID, Ordering::Release);
            self.available = false;
        }

        fn apply(&mut self, hotkey: HotKey, label: String) -> Result<(), String> {
            if self.hook_active && action_hotkeys::supports_launch_hotkey(hotkey) {
                self.unregister();
                return Ok(());
            }

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
            "catalog_actions" => {
                Some(("Actions catalog shortcut", &mut settings.catalog_actions_hotkey))
            }
            "audio_preview" => Some(("Preview audio shortcut", &mut settings.audio_preview_hotkey)),
            _ => None,
        }
    }

    fn sync_settings_ui(ui: &SpotlightWindow, settings: &AppSettings) {
        apply_palette_theme(ui, settings.theme);
        ui.set_hotkey_label(settings.hotkey.clone().into());
        ui.set_open_effect_controls_default(settings.open_effect_controls_by_default);
        ui.set_alternate_modifier(settings.alternate_modifier.as_str().into());
        ui.set_settings_hotkey_label(settings.settings_hotkey.clone().into());
        ui.set_catalog_all_hotkey_label(settings.catalog_all_hotkey.clone().into());
        ui.set_catalog_effects_hotkey_label(settings.catalog_effects_hotkey.clone().into());
        ui.set_catalog_project_hotkey_label(settings.catalog_project_hotkey.clone().into());
        ui.set_catalog_actions_hotkey_label(settings.catalog_actions_hotkey.clone().into());
        ui.set_audio_preview_hotkey_label(settings.audio_preview_hotkey.clone().into());
        ui.set_action_hotkeys(ModelRc::new(VecModel::from(Action::ALL.map(|action| ActionHotkeyUI {
            id: action.id().into(),
            label: action.name().into(),
            hint: action.hotkey_hint().into(),
            shortcut: settings.action_hotkeys.get(action.id()).map_or("", String::as_str).into(),
        }).to_vec())));
        action_hotkeys::update_bindings(settings);
        ui.set_auto_preview_audio(settings.auto_preview_audio);
        ui.set_config_path_display(settings::config_path().to_string_lossy().into_owned().into());
        if !ui.get_bridge_connected() {
            ui.set_status_message(format!("Ready · {}", settings.hotkey).into());
        }
    }

    fn save_and_sync_settings(
        ui: &SpotlightWindow,
        settings: &AppSettings,
        success_msg: String,
    ) -> Result<(), String> {
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
        sync_settings_ui(ui, settings);
        result
    }

    fn apply_hotkey_update(
        ui: &SpotlightWindow,
        state: &mut AppState,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
        hotkey: HotKey,
        label: String,
    ) -> Result<(), String> {
        if let Some(conflict) = settings::shortcut_conflict("launch", &label, &state.settings) {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(conflict.clone().into());
            sync_settings_ui(ui, &state.settings);
            return Err(conflict);
        }
        let result = hotkey_ctrl.borrow_mut().apply(hotkey, label.clone());
        match result {
            Ok(()) => {
                state.settings.hotkey = label.clone();
                save_and_sync_settings(
                    ui,
                    &state.settings,
                    format!("Saved shortcut: {label}"),
                )
            }
            Err(err) => {
                ui.set_settings_feedback_error(true);
                ui.set_settings_feedback(err.clone().into());
                sync_settings_ui(ui, &state.settings);
                Err(err)
            }
        }
    }

    fn update_action_hotkey(
        ui: &SpotlightWindow,
        state: &mut AppState,
        action: Action,
        label: Option<&str>,
    ) {
        let result = settings::save_action_hotkey(&settings::config_path(), &mut state.settings, action, label);
        ui.set_settings_feedback_error(result.is_err());
        let message = match result {
            Ok(()) => match label {
                Some(label) => format!("{} hotkey set to {label}.", action.name()),
                None => format!("Cleared {} hotkey.", action.name()),
            },
            Err(err) => err,
        };
        ui.set_settings_feedback(message.into());
        sync_settings_ui(ui, &state.settings);
    }

    fn toggle_default_effect_controls_setting(
        ui: &SpotlightWindow,
        state: &mut AppState,
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
        let _ = save_and_sync_settings(ui, &state.settings, msg);
    }

    fn reload_settings_live(
        ui: &SpotlightWindow,
        state: &mut AppState,
        hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
    ) {
        let (loaded, mut feedback, file_warning) = settings::load_settings_with_feedback();
        if file_warning.is_some() {
            // Keep the working settings instead of swapping in defaults from an unreadable file.
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(
                "Couldn't reload: the settings file has an error. Your current settings are unchanged."
                    .into(),
            );
            return;
        }
        state.settings.theme = loaded.theme;
        state.settings.open_effect_controls_by_default = loaded.open_effect_controls_by_default;
        state.settings.alternate_modifier = loaded.alternate_modifier;
        state.settings.settings_hotkey = loaded.settings_hotkey;
        state.settings.catalog_all_hotkey = loaded.catalog_all_hotkey;
        state.settings.catalog_effects_hotkey = loaded.catalog_effects_hotkey;
        state.settings.catalog_project_hotkey = loaded.catalog_project_hotkey;
        state.settings.catalog_actions_hotkey = loaded.catalog_actions_hotkey;
        state.settings.audio_preview_hotkey = loaded.audio_preview_hotkey;
        state.settings.auto_preview_audio = loaded.auto_preview_audio;
        state.settings.action_hotkeys.clear();

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
            if settings::shortcut_conflict("audio_preview", &state.settings.audio_preview_hotkey, &state.settings).is_some() {
                state.settings.audio_preview_hotkey.clear();
            }
            feedback.push(err);
        }
        state.settings.action_hotkeys = loaded.action_hotkeys;
        feedback.extend(settings::normalize_action_hotkeys(&mut state.settings));
        if feedback.is_empty() {
            ui.set_settings_feedback_error(false);
            ui.set_settings_feedback("Reloaded settings from config.json".into());
        } else {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(feedback.join(" ").into());
        }
        sync_settings_ui(ui, &state.settings);
    }

    fn toggle_settings_view(ui: &SpotlightWindow, state: &mut AppState, rec_flag: &AtomicBool) {
        let next = !ui.get_settings_open();
        stop_shortcut_recording(ui, Some(rec_flag));
        ui.set_settings_feedback("".into());
        ui.set_settings_open(next);
        if next && clear_marker_picker(ui, state).is_some() {
            refresh_marker_results(ui, state);
        }
        sync_audio_selection(ui, state);
        if !next {
            ui.invoke_focus_search();
        }
    }

    fn jump_to_catalog(ui: &SpotlightWindow, state: &mut AppState, target: &str) {
        if ui.get_active_catalog().as_str() == target {
            return;
        }
        clear_marker_picker(ui, state);
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

    let recording_hotkey_flag = Arc::new(AtomicBool::new(false));
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
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge = bridge.clone();
        let toast = error_toast.clone();
        ACTION_HOTKEY_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |binding, hwnd| {
                let Some(ui) = ui_weak.upgrade() else { return };
                let mut state = state_ref.borrow_mut();
                if binding.is_current(&state.settings) {
                    dispatch_action_hotkey(binding.action, &ui, &mut state, &bridge, &toast, Some(hwnd));
                }
            }));
        });
    }
    let rec_flag = recording_hotkey_flag.clone();
    let rec_flag_launch = recording_hotkey_flag.clone();
    let launch_pending = hotkey_pending.clone();
    let _action_hotkey_hook = match action_hotkeys::ActionHotkeyHook::install(
        move |binding, hwnd| {
            !rec_flag.load(Ordering::Acquire)
                && slint::invoke_from_event_loop(move || {
                    ACTION_HOTKEY_DISPATCH.with(|slot| {
                        if let Some(dispatch) = slot.borrow().as_ref() {
                            dispatch(binding, hwnd);
                        }
                    });
                })
                .is_ok()
        },
        move || {
            if rec_flag_launch.load(Ordering::Acquire) {
                return false;
            }
            launch_pending.store(true, Ordering::Release);
            slint::invoke_from_event_loop(|| {
                HOTKEY_DISPATCH.with(|slot| {
                    if let Some(callback) = slot.borrow_mut().as_mut() {
                        callback();
                    }
                });
            })
            .is_ok()
        },
    ) {
        Ok(hook) => Some(hook),
        Err(message) => {
            ui.set_action_hotkeys_warning(message.into());
            None
        }
    };
    let (hotkey_ctrl_raw, initial_hotkey_err) =
        HotkeyController::new(&initial_settings.hotkey, _action_hotkey_hook.is_some());
    let hotkey_id_ref = hotkey_ctrl_raw.active_id.clone();
    let hotkey_ctrl = Rc::new(RefCell::new(hotkey_ctrl_raw));
    {
        let st = state.borrow();
        sync_settings_ui(&ui, &st.settings);
        if let Some(err) = initial_hotkey_err {
            ui.set_settings_feedback_error(true);
            ui.set_settings_feedback(err.into());
        }
    }

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_select_theme(move |label| {
            if let Some(ui) = ui_weak.upgrade() {
                change_palette_theme(
                    &ui,
                    &mut state_ref.borrow_mut().settings,
                    label.as_str(),
                    &settings::config_path(),
                );
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_toggle_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                toggle_settings_view(&ui, &mut state_ref.borrow_mut(), &rec_flag);
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
                        let _ = apply_hotkey_update(&ui, &mut st, &hk_ctrl, hotkey, label);
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
                    if let Some(action) = Action::from_id(target_str) {
                        update_action_hotkey(&ui, &mut st, action, Some(&label));
                        return;
                    }
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
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_clear_shortcut(move |target| {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            stop_shortcut_recording(&ui, Some(&rec_flag));
            let mut st = state_ref.borrow_mut();
            if let Some(action) = Action::from_id(target.as_str()) {
                update_action_hotkey(&ui, &mut st, action, None);
                return;
            }
            if let Some((slot_name, slot)) =
                palette_shortcut_slot_mut(&mut st.settings, target.as_str())
            {
                slot.clear();
                let _ = save_and_sync_settings(
                    &ui,
                    &st.settings,
                    format!("Cleared {slot_name}."),
                );
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let hk_ctrl = hotkey_ctrl.clone();
        let rec_flag = recording_hotkey_flag.clone();
        ui.on_reset_default_hotkey(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            stop_shortcut_recording(&ui, Some(&rec_flag));
            if let Ok((hotkey, label)) = settings::parse_hotkey_spec(settings::DEFAULT_HOTKEY) {
                let mut st = state_ref.borrow_mut();
                let _ = apply_hotkey_update(&ui, &mut st, &hk_ctrl, hotkey, label);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
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
                format!("Alternate action modifier set to {}.", modifier_label(next)),
            );
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let rec_flag = recording_hotkey_flag.clone();
        let bridge = bridge.clone();
        let toast = error_toast.clone();
        ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta, repeat| {
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
                    toggle_settings_view(&ui, &mut state_ref.borrow_mut(), &rec_flag);
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
                Some(PaletteShortcutAction::PreviewAudio) => {
                    if !repeat {
                        ui.invoke_toggle_preview_playback();
                    }
                    true
                }
                Some(PaletteShortcutAction::RunAction(action)) => {
                    if !repeat {
                        dispatch_action_hotkey(
                            action, &ui, &mut state_ref.borrow_mut(), &bridge, &toast, None,
                        );
                    }
                    true
                }
                None => false,
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge = bridge.clone();
        ui.on_toggle_preview_playback(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let st = &mut *state_ref.borrow_mut();
                if st.is_visible && !ui.get_settings_open() {
                    st.audio_preview.toggle(&ui, &bridge, st.media_catalog.as_ref());
                }
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_seek_audio_preview(move |fraction| {
            if let Some(ui) = ui_weak.upgrade() {
                state_ref.borrow_mut().audio_preview.seek(&ui, fraction);
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_toggle_auto_preview_audio(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                st.settings.auto_preview_audio = !st.settings.auto_preview_audio;
                let message = if st.settings.auto_preview_audio {
                    "Auto-preview audio enabled."
                } else {
                    "Auto-preview audio disabled."
                };
                let _ = save_and_sync_settings(&ui, &st.settings, message.into());
            }
        });
    }
    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        ui.on_toggle_default_effect_controls(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                toggle_default_effect_controls_setting(&ui, &mut st);
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
        ui.on_reload_settings(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                reload_settings_live(&ui, &mut st, &hk_ctrl);
            }
        });
    }

    {
        let pending_flag = hotkey_pending.clone();
        let rec_flag = recording_hotkey_flag.clone();
        let ui_weak = ui.as_weak();
        GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
            if event.state != HotKeyState::Pressed
                || event.id != hotkey_id_ref.load(Ordering::Acquire)
            {
                return;
            }
            if rec_flag.load(Ordering::Acquire) {
                // Windows delivers the registered launch chord here instead of to the recorder.
                let _ = ui_weak.upgrade_in_event_loop(|ui| {
                    if ui.get_recording_hotkey() && ui.get_recording_target() != "launch" {
                        ui.set_settings_feedback_error(true);
                        ui.set_settings_feedback(format!(
                            "{} is already assigned to Launch Hotkey.", ui.get_hotkey_label(),
                        ).into());
                    }
                });
                return;
            }
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
                    debug_log::log_event(
                        LogKind::Focus,
                        "FOCUS",
                        format!("retry foreground (fg={:#x})", win_focus::get_foreground_hwnd()),
                    );
                    win_focus::force_foreground(our_hwnd);
                }
                FocusOutcome::HideLostFocus => {
                    debug_log::log_event(
                        LogKind::Focus,
                        "FOCUS",
                        format!("Lost focus -> hide palette (fg={:#x})", win_focus::get_foreground_hwnd()),
                    );
                    stop_shortcut_recording(&ui, Some(&rec_flag));
                    hide_palette(ui, &mut state_ref.borrow_mut());
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

    let quit_app: Rc<dyn Fn()> = {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let tray_ref = tray.clone();
        Rc::new(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let mut st = state_ref.borrow_mut();
                hide_palette(&ui, &mut st);
            }
            if let Some(ref mut t) = *tray_ref.borrow_mut() {
                t.remove();
            }
            let _ = slint::quit_event_loop();
        })
    };
    {
        let quit_on_premiere_exit = quit_app.clone();
        PREMIERE_EXIT_DISPATCH.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || quit_on_premiere_exit()));
        });
    }
    win_focus::spawn_premiere_exit_watcher(|| {
        let _ = slint::invoke_from_event_loop(|| {
            PREMIERE_EXIT_DISPATCH.with(|slot| {
                if let Some(mut callback) = slot.borrow_mut().take() {
                    callback();
                }
            });
        });
    });

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let bridge_ref = bridge.clone();
        let rec_flag = recording_hotkey_flag.clone();
        let quit_on_tray = quit_app.clone();
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
                    TrayAction::Quit => {
                        quit_on_tray();
                    }
                }
            }
        });
    }

    {
        let ui_weak = ui.as_weak();
        let state_ref = state.clone();
        let error_toast = error_toast.clone();
        let bridge = bridge.clone();
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
                finish_action(&ui, &mut st, None, &error_toast);
                poll_markers(&ui, &mut st, &bridge, &error_toast);
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
        state.pending_action.as_ref().map(|p| p.deadline),
        state.marker_picker.as_ref().and_then(MarkerPicker::deadline),
        state.marker_dialog.as_ref().map(MarkerDialog::deadline),
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
    static PRESET_CATALOG_DISPATCH: RefCell<Option<Box<dyn FnMut(Vec<EffectEntry>)>>> = const { RefCell::new(None) };
    static HOTKEY_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static ACTION_HOTKEY_DISPATCH: RefCell<Option<Box<dyn Fn(action_hotkeys::Binding, isize)>>> =
        const { RefCell::new(None) };
    static SYNC_FOCUS_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static DEADLINE_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    static PREMIERE_EXIT_DISPATCH: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
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
    const WINDOW_HEIGHT: i32 = 440;

    struct TestPlatform(Rc<MinimalSoftwareWindow>);

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn palette_outline_reaches_all_four_window_edges() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let (width, height) = (WINDOW_WIDTH as usize, WINDOW_HEIGHT as usize);
        adapter.set_size(PhysicalSize::new(width as u32, height as u32));
        ui.show().unwrap();

        let mut pixels = vec![Rgb8Pixel::default(); width * height];
        for (theme, outline) in [
            (PaletteTheme::Light, [0x8b, 0x95, 0xa5]),
            (PaletteTheme::Dark, [0x54, 0x54, 0x54]),
            (PaletteTheme::MidnightIndigo, [0x36, 0x39, 0x52]),
            (PaletteTheme::Premiere, [0x50, 0x50, 0x50]),
        ] {
            apply_palette_theme(&ui, theme);
            assert!(adapter.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, width);
            }));
            let [r, g, b] = outline;
            let border = Rgb8Pixel { r, g, b };
            for (x, y) in [
                (0, height / 2),
                (width - 1, height / 2),
                (width / 2, 0),
                (width / 2, height - 1),
            ] {
                assert_eq!(pixels[y * width + x], border, "{theme:?} outline at ({x}, {y})");
            }
        }
    }

    #[test]
    fn appearance_dropdown_saves_theme_and_recovers_from_save_failure() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();
        ui.set_settings_open(true);

        let dir =
            std::env::temp_dir().join(format!("premiere-spotlight-theme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        let target = Rc::new(RefCell::new(path.clone()));
        let saved = Rc::new(RefCell::new(AppSettings {
            auto_preview_audio: true,
            ..AppSettings::default()
        }));
        let weak = ui.as_weak();
        ui.on_select_theme({
            let (target, saved) = (target.clone(), saved.clone());
            move |label| {
                change_palette_theme(
                    &weak.unwrap(),
                    &mut saved.borrow_mut(),
                    label.as_str(),
                    &target.borrow(),
                );
            }
        });
        let weak = ui.as_weak();
        ui.on_toggle_settings(move || {
            let ui = weak.unwrap();
            ui.set_settings_open(!ui.get_settings_open());
        });

        let window = ui.window();
        let click = |x, y| {
            let position = LogicalPosition::new(x, y);
            for event in [
                WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                },
                WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                },
            ] {
                window.dispatch_event(event);
            }
        };
        let key = |key: Key| {
            window.dispatch_event(WindowEvent::KeyPressed { text: key.into() });
            window.dispatch_event(WindowEvent::KeyReleased { text: key.into() });
        };

        key(Key::RightArrow);
        key(Key::RightArrow);
        key(Key::Tab);
        key(Key::Return);
        key(Key::DownArrow);
        assert_eq!(ui.get_theme_name(), "Premiere");
        assert_eq!(*saved.borrow(), settings::load_settings_from(&path));
        key(Key::Escape);
        assert!(ui.get_settings_open());

        click(505., 136.);
        click(505., 138.);
        assert_eq!(ui.get_theme_name(), "Light");

        *target.borrow_mut() = dir.clone();
        click(505., 136.);
        key(Key::DownArrow);
        assert_eq!(ui.get_theme_name(), "Light");
        assert_eq!(saved.borrow().theme, PaletteTheme::Light);
        assert!(ui.get_settings_feedback_error());

        *target.borrow_mut() = path.clone();
        key(Key::DownArrow);
        assert_eq!(
            ui.get_theme_name(),
            "Dark",
            "the dropdown restored Light after the error"
        );
        assert!(!ui.get_settings_feedback_error());
        key(Key::Escape);
        assert!(ui.get_settings_open());
        key(Key::Escape);
        assert!(!ui.get_settings_open());

        let reloaded = AppSettings {
            theme: PaletteTheme::Premiere,
            ..saved.borrow().clone()
        };
        settings::save_settings_to(&path, &reloaded).unwrap();
        *saved.borrow_mut() = settings::load_settings_from(&path);
        apply_palette_theme(&ui, saved.borrow().theme);
        assert_eq!(ui.get_theme_name(), "Premiere");
        assert_eq!(ui.get_caption_color(), Color::from_rgb_u8(0x25, 0x25, 0x25));
        ui.set_settings_open(true);
        key(Key::RightArrow);
        key(Key::RightArrow);
        click(505., 136.);
        key(Key::UpArrow);
        assert_eq!(
            ui.get_theme_name(),
            "Midnight Indigo",
            "the dropdown reflected the reloaded theme"
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn audio_preview_resolves_only_the_settled_selection_and_ignores_stale_replies() {
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let item = |id: &str| ProjectItemEntry {
            item_id: id.into(),
            name: if id == "first" { "Renamed clip".into() } else { format!("{id}.wav") },
            bin_path: "Audio".into(),
            kind: ProjectItemKind::Media,
            offline: false,
        };
        let catalog = MediaCatalog {
            peer_id: 7,
            project_id: Some("project".into()),
            sequence_id: None,
            items: vec![item("first"), item("second")],
        };
        let bridge = BridgeClients::new();
        let outbound = bridge.connect_test_peer(catalog.clone());
        let mut preview = AudioPreview::default();
        let select = |preview: &mut AudioPreview, index: usize| {
            preview.select(&ui, Some(&catalog), Some(&catalog.items[index]), true);
        };
        select(&mut preview, 0);
        preview.poll(&ui, &bridge, Some(&catalog));
        assert!(outbound.try_recv().is_err());
        select(&mut preview, 1);
        assert!(ui.get_preview_active() && ui.get_preview_can_toggle());
        assert!(!ui.get_preview_playing());
        std::thread::sleep(Duration::from_millis(260));
        preview.poll(&ui, &bridge, Some(&catalog));
        let request: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
        assert_eq!(request["type"], "request_audio_preview");
        assert_eq!(request["projectId"], "project");
        assert_eq!(request["itemId"], "second");
        assert_eq!(request.get("sequenceId"), None);
        select(&mut preview, 0);
        preview.resolved(&ui, bridge::AudioPreviewResult {
            peer_id: 7,
            request_id: request["requestId"].as_u64().unwrap(),
            source: Some(bridge::AudioPreviewSource {
                path: "stale.wav".into(),
                in_seconds: None,
                out_seconds: None,
            }),
            message: String::new(),
        });
        assert!(!ui.get_preview_active());

        preview.toggle(&ui, &bridge, Some(&catalog));
        let manual: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
        assert_eq!(manual["itemId"], "first");
        preview.cancel(&ui);
        preview.resolved(&ui, bridge::AudioPreviewResult {
            peer_id: 7,
            request_id: manual["requestId"].as_u64().unwrap(),
            source: None,
            message: "Stale error".into(),
        });
        assert!(!ui.get_status_message().as_str().contains("Stale error"));

        preview.select(&ui, Some(&catalog), Some(&catalog.items[0]), false);
        std::thread::sleep(Duration::from_millis(260));
        preview.poll(&ui, &bridge, Some(&catalog));
        let identify: serde_json::Value = serde_json::from_str(&outbound.try_recv().unwrap()).unwrap();
        preview.resolved(&ui, bridge::AudioPreviewResult {
            peer_id: 7,
            request_id: identify["requestId"].as_u64().unwrap(),
            source: Some(bridge::AudioPreviewSource {
                path: "renamed.wav".into(),
                in_seconds: None,
                out_seconds: None,
            }),
            message: String::new(),
        });
        assert!(ui.get_preview_active() && ui.get_preview_can_toggle());
        assert!(!ui.get_preview_playing());
        assert!(ui.get_preview_message().is_empty());
    }

    #[test]
    fn inline_audio_controls_fill_from_the_left_and_keep_row_activation_separate() {
        use std::cell::Cell;
        use slint::platform::PointerEventButton;

        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(TestPlatform(adapter.clone()))).unwrap();
        let ui = SpotlightWindow::new().unwrap();
        let row = |name: &str| EffectItemUI {
            name: name.into(),
            category: "Media".into(),
            folder: "Audio".into(),
            badge_bg: Color::from_rgb_u8(0x21, 0x41, 0x68),
            badge_fg: Color::from_rgb_u8(0xb9, 0xe5, 0xff),
        };
        ui.set_results(ModelRc::new(VecModel::from(vec![
            row("Music.wav"),
            row("SFX.wav"),
        ])));
        ui.set_total_matches(2);
        ui.set_preview_active(true);
        ui.set_preview_time("0:01 / 0:04".into());
        ui.set_preview_progress(0.25);
        ui.set_preview_can_toggle(true);
        ui.set_preview_can_seek(true);
        adapter.set_size(PhysicalSize::new(WINDOW_WIDTH as u32, WINDOW_HEIGHT as u32));
        ui.show().unwrap();

        let width = WINDOW_WIDTH as usize;
        let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        let fill = Rgb8Pixel { r: 0xa5, g: 0xb4, b: 0xfc };
        assert_eq!(pixels[127 * width + 14], fill);
        assert_eq!(pixels[127 * width + 605], Rgb8Pixel { r: 0x3e, g: 0x42, b: 0x5a });
        assert_ne!(pixels[127 * width + 400], fill);
        assert_ne!(pixels[165 * width + 80], fill);
        // The progress fill and track meet the outline on y=129.
        assert_eq!(pixels[128 * width + 40], fill);
        assert_eq!(pixels[128 * width + 580], Rgb8Pixel { r: 0x3e, g: 0x42, b: 0x5a });

        let corner_pixels = |pixels: &[Rgb8Pixel]| {
            (127..130)
                .flat_map(|y| (6..12).chain(608..614).map(move |x| pixels[y * width + x]))
                .collect::<Vec<_>>()
        };
        let corners_with_track = corner_pixels(&pixels);
        ui.set_preview_can_seek(false);
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        assert_eq!(corner_pixels(&pixels), corners_with_track);
        ui.set_preview_can_seek(true);
        ui.set_preview_progress(1.0);
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        assert_eq!(corner_pixels(&pixels), corners_with_track);
        assert_eq!(pixels[128 * width + 605], fill);
        ui.set_preview_progress(0.25);
        let icon = Rgb8Pixel { r: 0xc7, g: 0xd2, b: 0xfe };
        let icon_pixels = |pixels: &[Rgb8Pixel]| {
            (102..121).flat_map(|y| &pixels[y * width + 574..y * width + 599])
                .filter(|&&pixel| pixel == icon).count()
        };
        assert!(icon_pixels(&pixels) > 0);
        ui.set_preview_playing(true);
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        assert!(icon_pixels(&pixels) > 0);

        for (theme, fill, track) in [
            (PaletteTheme::Light, [0x24, 0x5a, 0xc3], [0xaa, 0xb8, 0xce]),
            (PaletteTheme::Dark, [0xe0, 0xe0, 0xe0], [0x69, 0x69, 0x69]),
            (PaletteTheme::Premiere, [0x67, 0xb6, 0xff], [0x53, 0x61, 0x6f]),
        ] {
            apply_palette_theme(&ui, theme);
            assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
            let [r, g, b] = fill;
            assert_eq!(pixels[127 * width + 14], Rgb8Pixel { r, g, b });
            let [r, g, b] = track;
            assert_eq!(pixels[127 * width + 605], Rgb8Pixel { r, g, b });
        }

        let played = Rc::new(Cell::new(false));
        let position = Rc::new(Cell::new(None));
        let activated = Rc::new(Cell::new(None));
        let played_cb = played.clone();
        let seek_cb = position.clone();
        let activated_cb = activated.clone();
        ui.on_toggle_preview_playback(move || played_cb.set(true));
        ui.on_seek_audio_preview(move |fraction| seek_cb.set(Some(fraction)));
        ui.on_activate_selected(|| panic!("preview control activated the search result"));
        ui.on_activate_index(move |index| activated_cb.set(Some(index)));

        let click = |x, y| {
            let position = LogicalPosition::new(x, y);
            let button = PointerEventButton::Left;
            ui.window().dispatch_event(WindowEvent::PointerMoved { position });
            ui.window().dispatch_event(WindowEvent::PointerPressed { position, button });
            ui.window().dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        click(586., 111.);
        assert!(played.get());
        click(310., 127.);
        assert!(
            position.get().is_some_and(|value| (0.35..0.55).contains(&value)),
            "click produced seek position {:?}",
            position.get(),
        );
        assert_eq!(activated.get(), None);
        ui.set_preview_can_toggle(false);
        played.set(false);
        click(586., 111.);
        assert!(!played.get());
        assert_eq!(activated.get(), None);
        click(586., 149.);
        assert_eq!(activated.get(), Some(1));
        click(48., 111.);
        assert_eq!(activated.get(), Some(0));
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
        use slint::Model;

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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        ui.set_active_catalog("actions".into());
        ui.set_search_text("Remove Attributes".into());
        refresh_results_ui(&ui, &mut state);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(!state.is_visible);
        let request_id = state.pending_action.as_ref().unwrap().request_id;
        let command: serde_json::Value = serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
        assert_eq!(command["type"], "run_action");
        assert_eq!(command["action"], "remove_attributes");
        assert_eq!(command["requestId"], request_id);
        let toast = Rc::new(ErrorToast::default());
        for peer_id in [8, 7] {
            finish_action(&ui, &mut state, Some(ActionResult {
                peer_id, request_id, ok: true, message: "Removed effects".into(),
            }), &toast);
            assert_eq!(state.pending_action.is_some(), peer_id == 8);
        }
        assert_eq!(ui.get_status_message().as_str(), "Removed effects");
        assert!(state.recent_items.is_empty());

        for (catalog, action, wire_name) in [
            ("actions", Action::RegisterAdjustmentLayer, "register_adjustment_layer"),
            ("all", Action::AddAdjustmentLayer, "add_adjustment_layer"),
        ] {
            show_palette(&ui, &mut state);
            ui.set_active_catalog(catalog.into());
            ui.set_search_text(action.name().into());
            refresh_results_ui(&ui, &mut state);
            apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
            assert!(!state.is_visible);
            let command: serde_json::Value =
                serde_json::from_str(&requests.try_recv().unwrap()).unwrap();
            let pending = state.pending_action.as_ref().unwrap();
            assert_eq!(command["type"], "run_action");
            assert_eq!(command["action"], wire_name);
            assert_eq!(command["requestId"], pending.request_id);
            let request_id = pending.request_id;
            finish_action(&ui, &mut state, Some(ActionResult {
                peer_id: 7, request_id, ok: true, message: "Done".into(),
            }), &toast);
        }

        show_palette(&ui, &mut state);
        ui.set_active_catalog("all".into());
        ui.set_search_text("go to marker".into());
        refresh_results_ui(&ui, &mut state);
        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        assert!(state.is_visible && ui.get_marker_picker());
        assert_eq!(ui.get_marker_status().as_str(), "Loading markers…");

        let read_command = || -> serde_json::Value {
            serde_json::from_str(&requests.try_recv().unwrap()).unwrap()
        };
        let list_reply = |request: &serde_json::Value| {
            let json = serde_json::json!({
                "type": "marker_ack",
                "requestId": request["requestId"],
                "result": {
                    "kind": "list",
                    "projectId": "project",
                    "projectName": "Editing project",
                    "markers": [
                        { "name": "Cue", "comments": "Later", "ticks": "508032000000", "sequenceId": "sequence", "sequenceName": "Selects" },
                        { "name": "", "comments": "", "ticks": "0", "sequenceId": "nested", "sequenceName": "Nested" },
                        { "name": "Cue", "comments": "Earlier", "ticks": "254016000000", "sequenceId": "sequence", "sequenceName": "Selects" }
                    ]
                },
                "message": ""
            });
            let bridge::IncomingPluginMessage::MarkerAck { request_id, result, message } =
                serde_json::from_value(json).unwrap()
            else { panic!("Expected a marker reply") };
            MarkerEvent { peer_id: 7, request_id, result, message }
        };
        let discarded = read_command();
        assert_eq!(discarded["operation"], "list");
        escape_palette(&ui, &mut state);
        assert!(state.is_visible && !ui.get_marker_picker());
        assert_eq!(ui.get_search_text().as_str(), "go to marker");
        assert_eq!(ui.get_active_catalog().as_str(), "all");

        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        let active = read_command();
        receive_marker(&ui, &mut state, &bridge, list_reply(&discarded), &toast);
        assert_eq!(ui.get_marker_status().as_str(), "Loading markers…");
        receive_marker(&ui, &mut state, &bridge, list_reply(&active), &toast);
        assert_eq!(ui.get_marker_context().as_str(), "Editing project");
        let results = ui.get_results();
        assert_eq!(results.row_count(), 3);
        assert_eq!(results.row_data(0).unwrap().name.as_str(), "Unnamed marker");
        assert_eq!(results.row_data(0).unwrap().folder.as_str(), "Nested");
        assert_eq!(results.row_data(1).unwrap().folder.as_str(), "Selects");
        assert_eq!(results.row_data(1).unwrap().category.as_str(), "00:00:01.000");
        assert_eq!(results.row_data(2).unwrap().category.as_str(), "00:00:02.000");
        ui.set_search_text("Earlier".into());
        refresh_marker_results(&ui, &mut state);
        assert_eq!(ui.get_results().row_count(), 1);

        apply_entry_at(0, &ui, &mut state, &bridge).unwrap();
        let seek = read_command();
        assert_eq!(seek["operation"], "seek");
        assert_eq!(seek["projectId"], "project");
        assert_eq!(seek["marker"]["sequenceId"], "sequence");
        assert_eq!(seek["marker"]["ticks"], "254016000000");
        receive_marker(&ui, &mut state, &bridge, MarkerEvent {
            peer_id: 7,
            request_id: seek["requestId"].as_u64().unwrap(),
            result: Some(actions::markers::MarkerReply::Sought),
            message: String::new(),
        }, &toast);
        assert!(!state.is_visible && !ui.get_marker_picker());

        let mut picker = MarkerPicker::open(&bridge, "".into());
        let list = read_command();
        picker.receive(list_reply(&list));
        let mut other_timeline = MediaCatalog {
            peer_id: 7,
            project_id: Some("project".into()),
            sequence_id: Some("other-sequence".into()),
            items: vec![],
        };
        assert!(!picker.refresh_if_project_changed(&bridge, &other_timeline));
        other_timeline.project_id = Some("other-project".into());
        assert!(picker.refresh_if_project_changed(&bridge, &other_timeline));
        assert!(picker.markers().is_empty());
        assert!(picker.context_label.is_empty());
        assert_eq!(picker.status(), "Loading markers…");
        assert_eq!(read_command()["operation"], "list");
        assert!(!picker.accepts(&list_reply(&list)));

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

        activate_action(Action::RemoveAttributes, &ui, &mut state, &bridge).unwrap();
        assert_eq!(read_command()["action"], "remove_attributes");
        assert!(!state.is_visible);
        activate_action(Action::GoToMarker, &ui, &mut state, &bridge).unwrap();
        assert!(state.is_visible && ui.get_marker_picker());
        assert_eq!(read_command()["type"], "request_project_items");
        assert_eq!(read_command()["operation"], "list");
        escape_palette(&ui, &mut state);
        assert!(state.is_visible && !ui.get_marker_picker());
        assert_eq!(ui.get_active_catalog(), "actions");

        activate_action(Action::LockHorizontalAxis, &ui, &mut state, &bridge).unwrap();
        assert!(state.is_visible && ui.get_settings_open());
        assert_eq!(ui.get_settings_tab_index(), 1);
        assert!(requests.try_recv().is_err());

        ui.set_settings_open(false);
        state
            .settings
            .action_hotkeys
            .insert(Action::LockHorizontalAxis.id().into(), "F8".into());
        activate_action(Action::LockHorizontalAxis, &ui, &mut state, &bridge).unwrap();
        assert!(state.is_visible && !ui.get_settings_open());
        assert!(ui.get_status_message().as_str().contains("Hold F8"));
        assert!(requests.try_recv().is_err());
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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        let row_pos = LogicalPosition::new(120., 111.);
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
    fn catalog_tabs_cycle_and_reopen_to_recent_items() {
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
        let click_tab = |x| {
            let position = LogicalPosition::new(x, 69.);
            let button = PointerEventButton::Left;
            window.dispatch_event(WindowEvent::PointerPressed { position, button });
            window.dispatch_event(WindowEvent::PointerReleased { position, button });
        };
        let assert_highlight = |selected| {
            let width = WINDOW_WIDTH as usize;
            let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
            window.request_redraw();
            assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
            let tab_width = width / 4;
            let inactive = (selected + 1) % 4;
            let background = pixels[64 * width + inactive * tab_width + 12];
            let divider = Rgb8Pixel { r: 0x26, g: 0x28, b: 0x3a };
            assert_ne!(background, pixels[26 * width + 550]);
            assert_eq!(pixels[50 * width + 550], divider);
            assert_eq!(pixels[87 * width + 550], divider);
            for index in 0..4 {
                for x in [index * tab_width + 4, (index + 1) * tab_width - 5] {
                    for y in [54, 84] {
                        assert_eq!(pixels[y * width + x] != background, index == selected);
                    }
                }
                if index < 3 {
                    let boundary = (index + 1) * tab_width;
                    assert!(
                        pixels[64 * width + boundary - 1..=64 * width + boundary + 1]
                            .contains(&divider),
                        "missing divider near x={boundary}"
                    );
                }
            }
        };
        assert_highlight(0);
        for (index, expected) in [(1, "effects"), (2, "project"), (3, "actions"), (0, "all")] {
            press_tab();
            assert_eq!(ui.get_active_catalog().as_str(), expected);
            assert_eq!(ui.get_search_text().as_str(), "Take");
            assert_highlight(index);
        }
        click_tab(WINDOW_WIDTH as f32 - 5.);
        assert_eq!(ui.get_active_catalog().as_str(), "actions");
        assert_highlight(3);
        click_tab(WINDOW_WIDTH as f32 * 0.625);
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        assert_highlight(2);
        click_tab(WINDOW_WIDTH as f32 * 0.375);
        assert_eq!(ui.get_active_catalog().as_str(), "effects");
        click_tab(5.);
        assert_eq!(ui.get_active_catalog().as_str(), "all");
        click_tab(WINDOW_WIDTH as f32 * 0.375);
        press_tab();
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        assert_eq!(ui.get_search_text().as_str(), "Take");
        let mut state = AppState {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: vec![],
            recent_items: vec![],
            pending_effects: HashMap::new(),
            media_catalog: None,
            pending_placement: None,
            pending_source_open: None,
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        click_tab(39.);
        click_tab(WINDOW_WIDTH as f32 * 0.375);
        click_tab(WINDOW_WIDTH as f32 * 0.625);
        assert_eq!(ui.get_active_catalog().as_str(), "project");
        ui.set_status_message("This project item can't be placed on the timeline.".into());

        let width = WINDOW_WIDTH as usize;
        let mut pixels = vec![Rgb8Pixel::default(); width * WINDOW_HEIGHT as usize];
        window.request_redraw();
        assert!(adapter.draw_if_needed(|renderer| { renderer.render(&mut pixels, width); }));
        for row in 0..8 {
            let top = (94 + row * 38) * width;
            let bottom = top + 36 * width;
            assert!(pixels[top..bottom].iter().any(|pixel| pixel.r > 170), "row {row} should render its label");
        }
        assert!(pixels[404 * width..].iter().any(|pixel| pixel.r > 130), "the footer should render its text");
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
        let footer_pos = LogicalPosition::new(300., 423.);
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

        let footer_pos = LogicalPosition::new(300., 423.);
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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        use slint::Model;

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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        assert_eq!(
            ui.get_results().row_data(0).unwrap().folder,
            "Footage › B-Roll"
        );

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
        let with_folder = render("00_BACK UP", "04_Nests › 041_Main");

        let diff_xs: Vec<usize> = (95..129)
            .flat_map(|y| {
                (18..500).filter_map({
                    let without_folder = &without_folder;
                    let with_folder = &with_folder;
                    move |x| (without_folder[y * width + x] != with_folder[y * width + x]).then_some(x)
                })
            })
            .collect();
        assert!(!diff_xs.is_empty());
        // The category pill occupies the left edge of the content.
        let badge = Rgb8Pixel { r: 0x36, g: 0x26, b: 0x5c };
        let badge_end = (18..116)
            .rfind(|&x| without_folder[105 * width + x] == badge)
            .expect("category pill should be on the left");
        assert_eq!(without_folder[105 * width + 22], badge);

        // The folder text sits strictly to the right of the item name.
        let min_diff_x = *diff_xs.iter().min().unwrap();
        assert!(min_diff_x > badge_end + 60, "expected folder beside name, got min x={min_diff_x}");

        // A long name and folder leave the leading category pill intact.
        let long_base = render("Very_Long_Sequence_Name_That_Must_Elide_Cleanly_Before_The_Divider", "");
        let long_with_folder = render(
            "Very_Long_Sequence_Name_That_Must_Elide_Cleanly_Before_The_Divider",
            "01_Project › 04_Nests › 041_Main › Exports › Final_Master",
        );
        let leaked_into_badge = (95..129).any(|y| {
            (18..=badge_end)
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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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

        let click = |x, y| {
            let position = LogicalPosition::new(x, y);
            window.dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window.dispatch_event(WindowEvent::PointerReleased {
                position,
                button: PointerEventButton::Left,
            });
        };
        click(594., 422.);
        assert!(ui.get_settings_open());

        click(490., 142.);
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

        // About cancels recording and hides the shortcut controls.
        click(375., 72.);
        assert!(!ui.get_recording_hotkey());
        click(490., 142.);
        assert!(!ui.get_recording_hotkey());
        assert!(ui.get_settings_open());

        for _ in 0..3 {
            window.dispatch_event(WindowEvent::KeyPressed {
                text: Key::LeftArrow.into(),
            });
            window.dispatch_event(WindowEvent::KeyReleased {
                text: Key::LeftArrow.into(),
            });
        }
        click(490., 142.);
        assert!(ui.get_recording_hotkey());

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

        ui.set_action_hotkeys(ModelRc::new(VecModel::from(vec![ActionHotkeyUI {
            id: Action::AddAdjustmentLayer.id().into(),
            label: Action::AddAdjustmentLayer.name().into(),
            hint: Action::AddAdjustmentLayer.hotkey_hint().into(),
            shortcut: "F6".into(),
        }])));
        let cleared = Rc::new(RefCell::new(SharedString::default()));
        let cleared_cb = cleared.clone();
        ui.on_clear_shortcut(move |id| *cleared_cb.borrow_mut() = id);
        click(594., 422.);
        click(193., 72.);
        click(530., 128.);
        assert_eq!(ui.get_recording_target(), Action::AddAdjustmentLayer.id());
        window.dispatch_event(WindowEvent::KeyPressed { text: Key::F7.into() });
        window.dispatch_event(WindowEvent::KeyReleased { text: Key::F7.into() });
        assert_eq!(captured.borrow().last().unwrap().1, "F7");
        click(580., 128.);
        assert_eq!(cleared.borrow().as_str(), Action::AddAdjustmentLayer.id());
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
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            audio_preview: AudioPreview::default(),
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
        ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta, _repeat| {
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
                Some(PaletteShortcutAction::PreviewAudio | PaletteShortcutAction::RunAction(_)) => false,
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
