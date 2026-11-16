//! Replies from Premiere for effects, placements, Source Monitor opens, actions and markers.

use super::*;

/// Returns `Some(open_effect_controls)` if an effect apply just completed successfully.
pub(super) fn complete_effect(state: &mut AppState, result: &ApplyResult) -> Option<bool> {
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

pub(super) fn update_placement(
    ui: &SpotlightWindow,
    state: &mut AppState,
    reply: Option<CommandReply>,
) -> Option<SharedString> {
    let pending = state.pending_placement.as_ref()?;
    if let Some(result) = reply.filter(|reply| reply.answers(&pending.request)) {
        let pending = state.pending_placement.take().unwrap();
        if result.ok {
            if !pending.request.expired() {
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
    } else if pending.request.expired() {
        state.pending_placement = None;
        return Some("Placement outcome unknown. Check the timeline before retrying.".into());
    }
    None
}

pub(super) fn clear_marker_picker(ui: &SpotlightWindow, state: &mut AppState) -> Option<MarkerPicker> {
    let picker = state.marker_picker.take()?;
    ui.set_marker_picker(false);
    ui.set_marker_status("".into());
    ui.set_marker_context("".into());
    ui.set_search_text("".into());
    schedule_action_deadlines(state);
    Some(picker)
}

pub(super) fn escape_palette(ui: &SpotlightWindow, state: &mut AppState) {
    if let Some(picker) = clear_marker_picker(ui, state) {
        ui.set_search_text(picker.previous_query);
        refresh_marker_results(ui, state);
        ui.invoke_focus_search();
    } else {
        hide_and_restore_focus(ui, state);
    }
}

pub(super) fn refresh_marker_results(ui: &SpotlightWindow, state: &mut AppState) {
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);
}

pub(super) fn receive_marker(
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

pub(super) fn poll_markers(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients, toast: &Rc<ErrorToast>) {
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

pub(super) fn finish_action(
    ui: &SpotlightWindow,
    state: &mut AppState,
    reply: Option<CommandReply>,
    toast: &Rc<ErrorToast>,
) {
    let Some(pending) = &state.pending_action else {
        return;
    };
    let outcome = match reply.filter(|reply| reply.answers(pending)) {
        Some(reply) if reply.ok => Ok(reply.message),
        Some(reply) => Err(reply.message),
        None if pending.expired() => {
            Err("Premiere did not report the action outcome. Check the timeline.".to_string())
        }
        None => return,
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

pub(super) fn update_source_open(
    ui: &SpotlightWindow,
    state: &mut AppState,
    reply: Option<CommandReply>,
) -> Option<SharedString> {
    let pending = state.pending_source_open.as_ref()?;
    if let Some(result) = reply.filter(|reply| reply.answers(pending)) {
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
    } else if pending.expired() {
        state.pending_source_open = None;
        return Some("Source Monitor didn't respond. Check Premiere.".into());
    }
    None
}
