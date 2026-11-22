//! Replies from Premiere for effects, placements, Source Monitor opens, actions and markers.

use super::*;

/// Returns `Some(open_effect_controls)` if an effect apply just completed successfully.
pub(super) fn complete_effect(state: &mut AppState, result: &ApplyResult) -> Option<bool> {
    let request_id = match result.request_id {
        Some(id) => id,
        // Already loaded plugins can still acknowledge an apply without a request ID.
        None if result.ok => {
            let mut matches = state.pending_effects.iter().filter(|(_, pending)| {
                pending.request.peer_id == result.peer_id
                    && !pending.request.expired()
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
    let pending = state.pending_effects.get(&request_id)?;
    if !pending.request.is_answered_by(result.peer_id, request_id) || pending.request.expired() {
        return None;
    }
    if !result.ok {
        state.pending_effects.remove(&request_id);
        return None;
    }
    let completed = state.pending_effects.remove(&request_id).unwrap();
    let entry = completed.entry;
    state.catalog.record_usage(&entry);
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
            show_status(ui, result.message);
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

/// Turns the search box into the name field of Add Named Marker.
pub(super) fn start_marker_naming(ui: &SpotlightWindow, state: &mut AppState) {
    let previous_query = ui.get_search_text();
    clear_marker_picker(ui, state);
    state.marker_naming = Some(MarkerNaming { previous_query });
    ui.set_marker_naming(true);
    ui.set_search_text("".into());
    refresh_marker_results(ui, state);
    ui.invoke_focus_search();
}

/// Leaves the name field, if it is open, and returns what was searched before it.
pub(super) fn clear_marker_naming(ui: &SpotlightWindow, state: &mut AppState) -> Option<MarkerNaming> {
    let naming = state.marker_naming.take()?;
    ui.set_marker_naming(false);
    ui.set_search_text("".into());
    refresh_marker_results(ui, state);
    Some(naming)
}

/// Adds a marker named after the search text at the playhead, then returns to Premiere.
pub(super) fn submit_marker_name(
    ui: &SpotlightWindow,
    state: &mut AppState,
    bridge: &BridgeClients,
) -> Result<(), SharedString> {
    if state.pending_named_marker.is_some() {
        return Err("A marker is already being added.".into());
    }
    let facts = state.facts();
    if let Some(reason) = availability::primary(&SearchResult::Action(Action::AddNamedMarker), &facts).reason() {
        show_status(ui, reason);
        return Ok(());
    }
    let name = ui.get_search_text().trim().to_string();
    let request = bridge
        .marker_command(MarkerCommand::Add { name: name.clone() }, None)
        .map_err(SharedString::from)?;
    state.pending_named_marker = Some(PendingNamedMarker { request, name });
    clear_marker_naming(ui, state);
    hide_and_restore_focus(ui, state);
    schedule_action_deadlines(state);
    Ok(())
}

fn named_marker_added(name: &str) -> String {
    if name.is_empty() {
        "Added a marker at the playhead.".into()
    } else {
        format!("Added marker \u{201c}{name}\u{201d}.")
    }
}

pub(super) fn clear_marker_picker(ui: &SpotlightWindow, state: &mut AppState) -> Option<MarkerPicker> {
    clear_marker_naming(ui, state);
    let picker = state.marker_picker.take()?;
    ui.set_marker_picker(false);
    ui.set_marker_status("".into());
    ui.set_marker_context("".into());
    ui.set_search_text("".into());
    schedule_action_deadlines(state);
    Some(picker)
}

pub(super) fn escape_palette(ui: &SpotlightWindow, state: &mut AppState) {
    if let Some(naming) = clear_marker_naming(ui, state) {
        ui.set_search_text(naming.previous_query);
        refresh_marker_results(ui, state);
        ui.invoke_focus_search();
    } else if let Some(picker) = clear_marker_picker(ui, state) {
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
    } else if state
        .pending_named_marker
        .as_ref()
        .is_some_and(|pending| pending.request.is_answered_by(event.peer_id, event.request_id))
    {
        let pending = state.pending_named_marker.take().unwrap();
        if matches!(event.result, Some(MarkerReply::Added)) {
            let message = named_marker_added(&pending.name);
            debug_log::log_event(LogKind::Success, "MARKER", &message);
            show_status(ui, message);
        } else {
            let message = if event.message.is_empty() {
                "Premiere could not add the marker.".to_string()
            } else {
                event.message
            };
            debug_log::log_event(LogKind::Error, "MARKER", &message);
            toast.show(message.into(), state.previous_hwnd);
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
    if state.pending_named_marker.as_ref().is_some_and(|pending| pending.request.expired()) {
        state.pending_named_marker = None;
        toast.show(
            "Premiere didn't confirm the marker. Check the Timeline before retrying.".into(),
            state.previous_hwnd,
        );
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
            show_status(ui, message);
        }
        Err(message) => {
            debug_log::log_event(LogKind::Error, "ACTION", &message);
            toast.show(message.into(), state.previous_hwnd);
        }
    }
}

/// Handles the reply to opening an item in the Source Monitor or a sequence in the
/// Timeline, or the timeout when `reply` is `None`. Returns an error to show.
pub(super) fn update_open(
    ui: &SpotlightWindow,
    state: &mut AppState,
    reply: Option<CommandReply>,
) -> Option<SharedString> {
    let pending = state.pending_open.as_ref()?;
    let target = pending.target;
    if let Some(result) = reply.filter(|reply| reply.answers(&pending.request)) {
        state.pending_open = None;
        if result.ok {
            show_status(ui, result.message);
        } else {
            return Some(if result.message.is_empty() {
                target.failure().into()
            } else {
                result.message.into()
            });
        }
    } else if pending.request.expired() {
        state.pending_open = None;
        return Some(target.timeout().into());
    }
    None
}
