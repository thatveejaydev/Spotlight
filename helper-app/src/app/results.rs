//! The result list: search, recent items, bin drill-down and the visible row slice.

use super::*;

/// How a kind of result looks: its icon (named in `ui/icons.slint`), its color on the
/// dark themes, and the darker color the Light theme draws it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KindIcon {
    pub name: &'static str,
    pub tint: (u8, u8, u8),
    pub ink: (u8, u8, u8),
}

const fn kind(name: &'static str, tint: (u8, u8, u8), ink: (u8, u8, u8)) -> KindIcon {
    KindIcon { name, tint, ink }
}

pub(super) fn kind_icon(result: &SearchResult) -> KindIcon {
    const VIDEO_FILE: KindIcon = kind("video", (125, 211, 252), (3, 105, 161));
    const IMAGE_FILE: KindIcon = kind("image", (94, 234, 212), (15, 118, 110));
    const AUDIO_FILE: KindIcon = kind("audio", (190, 242, 100), (77, 124, 15));
    const OTHER: KindIcon = kind("other", (203, 213, 225), (71, 85, 105));
    match result {
        SearchResult::Effect(entry) => match entry.effect_type {
            EffectType::VideoEffect => kind("video-effect", (165, 180, 252), (79, 70, 229)),
            EffectType::AudioEffect => kind("audio-effect", (110, 231, 183), (4, 120, 87)),
            EffectType::VideoTransition => kind("video-transition", (252, 211, 77), (161, 98, 7)),
            EffectType::AudioTransition => kind("audio-transition", (249, 168, 212), (190, 24, 93)),
            EffectType::Preset => kind("preset", (255, 174, 162), (194, 65, 12)),
            EffectType::Unknown => OTHER,
        },
        SearchResult::ProjectItem(item) => match item.kind {
            ProjectItemKind::Media => match item.media_type {
                Some(MediaType::Image) => IMAGE_FILE,
                Some(MediaType::Audio) => AUDIO_FILE,
                Some(MediaType::Video | MediaType::Unknown) | None => VIDEO_FILE,
            },
            ProjectItemKind::Sequence | ProjectItemKind::ActiveSequence => {
                kind("sequence", (216, 180, 254), (126, 34, 206))
            }
            ProjectItemKind::Bin => kind("bin", (253, 224, 131), (161, 98, 7)),
            ProjectItemKind::Other => OTHER,
        },
        SearchResult::Action(_) => kind("action", (253, 186, 116), (194, 65, 12)),
        SearchResult::Marker(_) => kind("marker", (252, 165, 165), (185, 28, 28)),
        SearchResult::LibraryFile(file) => match file.media_type {
            MediaType::Image => IMAGE_FILE,
            MediaType::Audio => AUDIO_FILE,
            MediaType::Video | MediaType::Unknown => VIDEO_FILE,
        },
    }
}

/// The kind of effect in words, shown at the right of effect rows.
fn effect_kind(kind: EffectType) -> &'static str {
    match kind {
        EffectType::VideoEffect => "Video effect",
        EffectType::AudioEffect => "Audio effect",
        EffectType::VideoTransition => "Video transition",
        EffectType::AudioTransition => "Audio transition",
        EffectType::Preset => "Preset",
        EffectType::Unknown => "Effect",
    }
}

fn bin_path_label(path: &str) -> String {
    path.replace(crate::catalog::BIN_PATH_SEPARATOR, " › ")
}

/// One row of the result list. A Library file the project holds is checked as its clip.
pub(super) fn result_row(result: &SearchResult, facts: &Facts, catalog: &CatalogStore) -> EffectItemUI {
    let resolved = catalog.resolve(result);
    let reason = availability::primary(&resolved, facts).reason();
    let icon = kind_icon(result);
    let needs_import = matches!(resolved, SearchResult::LibraryFile(_));
    let color = |(r, g, b): (u8, u8, u8)| Color::from_rgb_u8(r, g, b);
    let (name, detail, subtitle): (&str, String, &str) = match result {
        SearchResult::Marker(marker) => (
            marker.title(),
            format!("{} · {}", marker.sequence_name, marker.position()),
            "",
        ),
        SearchResult::Action(action) => (action.name(), "Action".into(), action.description()),
        SearchResult::Effect(entry) if entry.is_preset() => (
            entry.name.as_str(),
            if entry.folder.is_empty() { "Preset".into() } else { bin_path_label(&entry.folder) },
            "Finds it in the Effects panel so you can drag it onto a clip",
        ),
        SearchResult::Effect(entry) => (entry.name.as_str(), effect_kind(entry.effect_type).into(), ""),
        SearchResult::ProjectItem(entry) => (entry.name.as_str(), bin_path_label(&entry.bin_path), ""),
        SearchResult::LibraryFile(file) => (
            file.name.as_str(),
            if matches!(resolved, SearchResult::ProjectItem(_)) {
                format!("In project · {}", bin_path_label(&file.folder))
            } else {
                bin_path_label(&file.folder)
            },
            "",
        ),
    };
    EffectItemUI {
        name: name.into(),
        detail: detail.into(),
        subtitle: reason.unwrap_or(subtitle).into(),
        icon: icon.name.into(),
        tint: color(icon.tint),
        ink: color(icon.ink),
        disabled: reason.is_some(),
        needs_import,
    }
}

/// What Enter, the alternate modifier with Enter, and Esc do for the highlighted row,
/// for the footer. An empty label hides that hint.
pub(super) fn key_hints(state: &AppState) -> (&'static str, &'static str, &'static str) {
    if state.marker_naming.is_some() {
        return ("Add marker", "", "Back");
    }
    let escape = if state.marker_picker.is_some() { "Back" } else { "Close" };
    let Some(result) = state.matched_items.get(state.selected_abs_index) else {
        return ("", "", escape);
    };
    let result = &state.catalog.resolve(result);
    let facts = state.facts();
    let ready = |availability: Availability| availability.reason().is_none();
    let open_effect_controls = state.settings.open_effect_controls_by_default;
    let (enter, alternate) = match result {
        SearchResult::Marker(_) => ("Go to marker", ""),
        SearchResult::Action(Action::GoToMarker | Action::AddNamedMarker) => ("Open", ""),
        SearchResult::Action(Action::LockHorizontalAxis) => ("Show how", ""),
        SearchResult::Action(_) => ("Run", ""),
        SearchResult::Effect(entry) if entry.is_preset() => ("Find in Effects panel", ""),
        SearchResult::Effect(_) if open_effect_controls => ("Apply and open Effect Controls", "Apply"),
        SearchResult::Effect(_) => ("Apply", "Apply and open Effect Controls"),
        SearchResult::ProjectItem(item) => match item.kind {
            ProjectItemKind::Bin => ("Open bin", ""),
            ProjectItemKind::Media => ("Place at playhead", "Open in Source Monitor"),
            ProjectItemKind::Sequence | ProjectItemKind::ActiveSequence => ("Nest at playhead", "Open in Timeline"),
            ProjectItemKind::Other => ("", ""),
        },
        // Only files the project doesn't hold; the rest resolved to their clip above.
        SearchResult::LibraryFile(_) => ("Import and place at playhead", "Import to Source Monitor"),
    };
    let enter = if ready(availability::primary(result, &facts)) { enter } else { "" };
    let alternate = if ready(availability::alternate(result, &facts)) { alternate } else { "" };
    (enter, alternate, escape)
}

pub(super) fn sync_key_hints(ui: &SpotlightWindow, state: &AppState) {
    let (enter, alternate, escape) = key_hints(state);
    ui.set_hint_enter(enter.into());
    ui.set_hint_alternate(alternate.into());
    ui.set_hint_escape(escape.into());
}

pub(super) fn remember_item(state: &mut AppState, recent: RecentItem) {
    state.recent_items.retain(|entry| {
        entry.project_id != recent.project_id || !entry.item.same_item(&recent.item)
    });
    state.recent_items.insert(0, recent);
    state.recent_items.truncate(MAX_RECENT_ITEMS);
}

pub(super) fn recent_results(state: &AppState) -> Vec<SearchResult> {
    let in_project = |recent: &RecentItem| {
        state
            .media_catalog
            .as_ref()
            .filter(|catalog| recent.project_id == catalog.project_id)
    };
    let mut results: Vec<SearchResult> = Vec::new();
    for recent in &state.recent_items {
        let result = match &recent.item {
            SearchResult::Effect(_) => Some(recent.item.clone()),
            SearchResult::Action(_) | SearchResult::Marker(_) => None,
            SearchResult::ProjectItem(item) => in_project(recent).and_then(|catalog| {
                catalog
                    .items
                    .iter()
                    .find(|entry| entry.item_id == item.item_id && entry.placement_error().is_none())
                    .cloned()
                    .map(Arc::new)
                    .map(SearchResult::ProjectItem)
            }),
            // A placed Library file is a project clip now; show that clip, as All does.
            SearchResult::LibraryFile(file) => in_project(recent).and_then(|_| {
                let file = state.catalog.library_file(&file.key)?;
                Some(state.catalog.resolve(&SearchResult::LibraryFile(file)))
            }),
        };
        if let Some(result) = result.filter(|result| !results.iter().any(|seen| seen.same_item(result))) {
            results.push(result);
        }
    }
    results
}

/// Asks the plugin for the Premiere state that decides which rows are dimmed.
/// Keeps the current state until the reply arrives.
pub(super) fn request_premiere_context(state: &mut AppState, bridge: &BridgeClients) {
    state.context_request = bridge
        .request_context()
        .map(|(request_id, peer_id)| PendingRequest::new(peer_id, request_id));
}

/// Stores the plugin's reply to the latest context request and redraws the rows.
pub(super) fn receive_premiere_context(ui: &SpotlightWindow, state: &mut AppState, event: ContextEvent) {
    if !state
        .context_request
        .is_some_and(|request| request.is_answered_by(event.peer_id, event.request_id))
    {
        return;
    }
    state.context_request = None;
    state.premiere_context = event.context;
    state.results_dirty = true;
    sync_viewport_slice(ui, state);
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
    show_status(ui, format!("Inside {} · Backspace goes up a level", bin_path));
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
        show_status(ui, format!("Inside {} · Backspace goes up a level", parent));
    } else {
        show_status(ui, "Showing all project media");
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
            Some(SearchResult::ProjectItem(item)) => Some(PreviewItem::Project(item.as_ref())),
            Some(SearchResult::LibraryFile(file)) => Some(PreviewItem::File(&file.path)),
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
    let show_history = state.marker_naming.is_none()
        && state.active_bin_path.is_none()
        && catalog == Catalog::All
        && query.trim().is_empty();
    ui.set_show_recent_items(show_history);
    let started = Instant::now();
    // The search box holds a marker name, so nothing is searched.
    state.matched_items = if state.marker_naming.is_some() {
        Vec::new()
    } else if let Some(picker) = &state.marker_picker {
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
        let (effects_count, media_count, library_count) = state.catalog.counts();
        debug_log::record_search_timing(
            elapsed,
            query.as_str(),
            catalog.id(),
            total,
            if let Some(picker) = &state.marker_picker {
                picker.markers().len()
            } else if catalog == Catalog::Actions {
                Action::ALL.len()
            } else if catalog == Catalog::Library {
                library_count
            } else if catalog == Catalog::All && !show_history {
                effects_count + media_count + library_count + Action::ALL.len()
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
        sync_key_hints(ui, state);
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
    sync_key_hints(ui, state);
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
    let facts = state.facts();
    let slice: Vec<EffectItemUI> = state.matched_items[state.scroll_offset..end]
        .iter()
        .map(|entry| result_row(entry, &facts, &state.catalog))
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
