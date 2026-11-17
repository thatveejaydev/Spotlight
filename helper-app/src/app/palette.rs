//! Showing, hiding, theming and focusing the palette window.

use super::*;

pub(super) fn extract_window_hwnd(ui: &SpotlightWindow) -> isize {
    win_focus::window_hwnd(ui.window())
}

pub(super) fn stop_shortcut_recording(ui: &SpotlightWindow, rec_flag: Option<&AtomicBool>) {
    ui.set_recording_hotkey(false);
    ui.set_recording_target("".into());
    if let Some(flag) = rec_flag {
        flag.store(false, Ordering::Release);
    }
}

pub(super) fn center_and_show(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients) {
    stop_shortcut_recording(ui, None);
    ui.set_settings_open(false);
    ui.set_settings_feedback("".into());
    clear_marker_picker(ui, state);
    set_catalog(ui, Catalog::All);
    ui.set_search_text(SharedString::from(""));
    clear_bin_scope(ui, state);
    // Keep the last project list visible; the plugin replies at once if nothing changed.
    bridge.request_project_items();
    ui.set_project_loading(bridge.project_scan_pending());
    // The selection may have changed since the last open, so rows stay enabled until Premiere answers.
    state.premiere_context = None;
    ui.set_flash_row(-1);
    request_premiere_context(state, bridge);
    state.selected_abs_index = 0;
    state.scroll_offset = 0;
    refresh_results_ui(ui, state);

    show_palette(ui, state);
}

pub(super) fn show_palette(ui: &SpotlightWindow, state: &mut AppState) {
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

pub(super) fn update_palette_caption(ui: &SpotlightWindow, hwnd: isize) {
    let color = ui.get_caption_color();
    win_focus::set_palette_caption_color(hwnd, [color.red(), color.green(), color.blue()]);
}

pub(super) fn apply_palette_theme(ui: &SpotlightWindow, theme: PaletteTheme) {
    ui.set_theme_name(theme.label().into());
    update_palette_caption(ui, extract_window_hwnd(ui));
}

pub(super) fn change_palette_theme(ui: &SpotlightWindow, settings: &mut AppSettings, label: &str, path: &Path) {
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

pub(super) fn restore_premiere_focus(hwnd: isize) {
    if hwnd != 0
        && win_focus::get_foreground_hwnd() != hwnd
        && win_focus::is_premiere_active_and_unminimized(hwnd)
    {
        win_focus::restore_foreground(hwnd);
    }
}

pub(super) fn hide_palette(ui: &SpotlightWindow, state: &mut AppState) {
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

pub(super) fn hide_and_restore_focus(ui: &SpotlightWindow, state: &mut AppState) {
    restore_premiere_focus(state.previous_hwnd);
    hide_palette(ui, state);
}

pub(super) fn toggle_palette_from_hotkey(ui: &SpotlightWindow, state: &mut AppState, bridge: &BridgeClients) {
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

pub(super) fn toggle_palette_from_hotkey_with_focus(
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
            format!("{} -> hide palette", state.settings.hotkey),
        );
        hide_and_restore_focus(ui, state);
        return;
    }
    if is_premiere_fg {
        debug_log::log_event(
            LogKind::Focus,
            "HOTKEY",
            format!("{} -> show palette", state.settings.hotkey),
        );
        state.previous_hwnd = fg_hwnd;
        center_and_show(ui, state, bridge);
        return;
    }
    if state.is_visible {
        debug_log::log_event(
            LogKind::Focus,
            "HOTKEY",
            format!("{} -> clear stale visibility", state.settings.hotkey),
        );
        hide_and_restore_focus(ui, state);
    }
}

pub(super) enum FocusOutcome {
    KeepVisible,
    RetryForeground,
    HideLostFocus,
}

pub(super) fn evaluate_palette_focus(state: &mut AppState, is_our_window: bool, now: Instant) -> FocusOutcome {
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
