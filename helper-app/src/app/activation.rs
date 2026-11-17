//! Activating a result, an action or an action hotkey.

use super::*;

pub(super) fn apply_entry_at(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<Option<PresetHandoff>, SharedString> {
    let open_effect_controls =
        settings::resolve_open_effect_controls(state.settings.open_effect_controls_by_default, false);
    apply_entry_with_options(abs_index, ui, state, bridge, open_effect_controls)
}

pub(super) fn apply_entry_with_options(
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
    open_effect_controls: bool,
) -> Result<Option<PresetHandoff>, SharedString> {
    let Some(chosen) = state.matched_items.get(abs_index).cloned() else {
        return Ok(None);
    };
    if block_unavailable(availability::primary(&chosen, &state.facts()), abs_index, ui, state) {
        return Ok(None);
    }
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
                request: PendingRequest::new(catalog.peer_id, request_id),
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

pub(super) fn activate_action(
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
                set_catalog(ui, Catalog::Actions);
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
            state.pending_action = Some(PendingRequest::new(peer_id, request_id));
        }
    }
    if action != Action::GoToMarker && action != Action::LockHorizontalAxis && state.is_visible {
        hide_and_restore_focus(ui, state);
    }
    schedule_action_deadlines(state);
    Ok(())
}

pub(super) fn dispatch_action_hotkey(
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

pub(super) fn shift_activate_at(
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
        SearchResult::ProjectItem(ref entry) => {
            if block_unavailable(availability::alternate(&chosen, &state.facts()), abs_index, ui, state) {
                return Ok(None);
            }
            let Some(catalog) = &state.media_catalog else {
                return Err("Project media changed. Search again.".into());
            };
            let request_id = bridge
                .open_in_source_monitor(catalog, entry.item_id.clone())
                .map_err(SharedString::from)?;
            ui.set_status_message(format!("Opening {}…", entry.name).into());
            state.pending_source_open = Some(PendingRequest::new(catalog.peer_id, request_id));
            schedule_action_deadlines(state);
            hide_palette(ui, state);
            restore_premiere_focus(state.previous_hwnd);
            Ok(None)
        }
    }
}

/// How long a disabled row's outline flashes after activating it.
const FLASH_DURATION: Duration = Duration::from_millis(400);

/// Keeps the palette open and shows why when `availability` blocks the row at
/// `abs_index`. Returns true when it blocked.
fn block_unavailable(
    availability: Availability,
    abs_index: usize,
    ui: &SpotlightWindow,
    state: &AppState,
) -> bool {
    let Some(reason) = availability.reason() else {
        return false;
    };
    ui.set_status_message(reason.into());
    debug_log::log_event(LogKind::Info, "UNAVAILABLE", reason);
    if let Some(row) = abs_index.checked_sub(state.scroll_offset).filter(|row| *row < VISIBLE_ROWS) {
        ui.set_flash_row(row as i32);
        let weak = ui.as_weak();
        Timer::single_shot(FLASH_DURATION, move || {
            if let Some(ui) = weak.upgrade() {
                if ui.get_flash_row() == row as i32 {
                    ui.set_flash_row(-1);
                }
            }
        });
    }
    true
}

pub(super) fn finish_activation(
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
