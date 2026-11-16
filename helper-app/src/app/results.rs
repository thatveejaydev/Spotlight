//! The result list: search, recent items, bin drill-down and the visible row slice.

use super::*;

pub(super) fn badge_colors(effect_type: &str) -> (Color, Color) {
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

pub(super) fn remember_item(state: &mut AppState, recent: RecentItem) {
    state.recent_items.retain(|entry| {
        entry.project_id != recent.project_id || !entry.item.same_item(&recent.item)
    });
    state.recent_items.insert(0, recent);
    state.recent_items.truncate(MAX_RECENT_ITEMS);
}

pub(super) fn recent_results(state: &AppState) -> Vec<SearchResult> {
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

pub(super) fn sync_bin_ui(ui: &SpotlightWindow, state: &AppState) {
    ui.set_active_bin(state.active_bin_path.as_deref().unwrap_or("").into());
}

pub(super) fn clear_bin_scope(ui: &SpotlightWindow, state: &mut AppState) {
    state.active_bin_path = None;
    sync_bin_ui(ui, state);
}

pub(super) fn enter_bin(entry: &ProjectItemEntry, ui: &SpotlightWindow, state: &mut AppState) {
    let Some(bin_path) = entry.child_bin_path() else {
        return;
    };
    state.active_bin_path = Some(bin_path.clone());
    sync_bin_ui(ui, state);
    set_catalog(ui, Catalog::Project);
    ui.set_search_text(SharedString::from(""));
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
    ui.set_status_message(format!("Browsing {} · Backspace to go up", bin_path).into());
    ui.invoke_focus_search();
}

pub(super) fn step_out_bin(ui: &SpotlightWindow, state: &mut AppState) {
    let Some(current) = state.active_bin_path.take() else {
        return;
    };
    state.active_bin_path = crate::catalog::parent_bin_path(&current).map(str::to_string);
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

pub(super) fn refresh_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    rebuild_results_ui(ui, state);
    sync_audio_selection(ui, state);
}

pub(super) fn sync_audio_selection(ui: &SpotlightWindow, state: &mut AppState) {
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

pub(super) fn rebuild_results_ui(ui: &SpotlightWindow, state: &mut AppState) {
    let query = ui.get_search_text();
    let catalog = current_catalog(ui);
    let show_history =
        state.active_bin_path.is_none() && catalog == Catalog::All && query.trim().is_empty();
    ui.set_show_recent_items(show_history);
    let started = Instant::now();
    state.matched_items = if let Some(picker) = &state.marker_picker {
        state.catalog.search_markers(query.as_str(), picker.markers())
    } else if show_history {
        recent_results(state)
    } else {
        state.catalog.search_in_bin(
            query.as_str(),
            catalog,
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
            catalog.id(),
            total,
            if let Some(picker) = &state.marker_picker {
                picker.markers().len()
            } else if catalog == Catalog::Actions {
                Action::ALL.len()
            } else if catalog == Catalog::All && !show_history {
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

pub(super) fn refresh_catalog_ui(ui: &SpotlightWindow, state: &mut AppState, preserve_media: bool) {
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

pub(super) fn sync_viewport_slice(ui: &SpotlightWindow, state: &mut AppState) {
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
                    badge_colors(entry.effect_type.id()),
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
                    folder.replace(crate::catalog::BIN_PATH_SEPARATOR, " › ").into()
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

/// The catalog tab the palette shows.
pub(super) fn current_catalog(ui: &SpotlightWindow) -> Catalog {
    // The Slint tabs only ever set one of the catalog ids.
    Catalog::from_id(ui.get_active_catalog().as_str()).unwrap_or(Catalog::All)
}

pub(super) fn set_catalog(ui: &SpotlightWindow, catalog: Catalog) {
    ui.set_active_catalog(catalog.id().into());
}
