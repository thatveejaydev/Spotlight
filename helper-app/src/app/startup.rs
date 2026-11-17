//! Builds the palette window and connects every UI callback, bridge event,
//! hotkey, and window hook to the shared app state. `main` runs the
//! preflight checks and then calls [`run`].

use super::*;
use std::cell::Cell;

/// What most callbacks need. Cloning is cheap: every field is a shared handle.
#[derive(Clone)]
struct AppCtx {
    ui: slint::Weak<SpotlightWindow>,
    state: Rc<RefCell<AppState>>,
    bridge: BridgeClients,
    toast: Rc<ErrorToast>,
    /// True while the settings view is recording a shortcut.
    recording: Arc<AtomicBool>,
}

impl AppCtx {
    /// Runs `f` with the live window and a mutable borrow of the state.
    /// Does nothing once the window is gone.
    fn with_state(&self, f: impl FnOnce(&SpotlightWindow, &mut AppState)) {
        if let Some(ui) = self.ui.upgrade() {
            f(&ui, &mut self.state.borrow_mut());
        }
    }
}

pub(crate) fn run() -> Result<(), slint::PlatformError> {
    select_backend()?;
    let ui = SpotlightWindow::new()?;
    ui.set_app_version(env!("CARGO_PKG_VERSION").into());
    ui.set_default_hotkey(settings::DEFAULT_HOTKEY.into());
    let error_toast = Rc::new(ErrorToast::default());
    let debug_log_ctrl = Rc::new(DebugLogController::default());
    win_focus::init_palette_window(extract_window_hwnd(&ui));
    debug_log::log_event(LogKind::Info, "INIT", "Premiere Spotlight helper started");

    let initial_settings = load_initial_settings(&error_toast);

    // Create native Windows System Tray icon (no taskbar icon)
    let tray = Rc::new(RefCell::new(SystemTray::new()));

    let state = Rc::new(RefCell::new(AppState::new(initial_settings.clone())));
    refresh_results_ui(&ui, &mut state.borrow_mut());

    let ctx = AppCtx {
        ui: ui.as_weak(),
        state,
        // Localhost WebSocket bridge on 127.0.0.1:3847
        bridge: BridgeClients::new(),
        toast: error_toast,
        recording: Arc::new(AtomicBool::new(false)),
    };

    wire_preset_library(&ctx);
    wire_audio_preview_poll(&ctx);
    wire_bridge(&ctx);
    wire_palette(&ui, &ctx, &debug_log_ctrl);

    // Set from hook threads, consumed on the UI thread.
    let launch_pending = Arc::new(AtomicBool::new(false));
    wire_launch_dispatch(&ctx, &launch_pending);
    let _action_hotkey_hook = install_action_hotkey_hook(&ui, &ctx, &launch_pending);
    let hotkey_ctrl = create_hotkey_controller(
        &ui,
        &ctx,
        &initial_settings.hotkey,
        _action_hotkey_hook.is_some(),
    );
    wire_settings(&ui, &ctx, &hotkey_ctrl);
    wire_registered_hotkey_events(&ctx, hotkey_ctrl.borrow().active_id.clone(), launch_pending);

    let _foreground_hook = wire_focus_tracking(&ctx);
    let quit_app = make_quit_app(&ctx, tray);
    wire_premiere_exit(quit_app.clone());
    wire_tray(&ctx, quit_app);
    wire_deadlines(&ctx);

    // Keep event loop alive while helper is active in the System Tray
    slint::run_event_loop_until_quit()
}

fn select_backend() -> Result<(), slint::PlatformError> {
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
    Ok(())
}

fn load_initial_settings(error_toast: &Rc<ErrorToast>) -> AppSettings {
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
    initial_settings
}

// ---------------------------------------------------------------------------
// Presets, audio preview, and the Premiere bridge
// ---------------------------------------------------------------------------

fn wire_preset_library(ctx: &AppCtx) {
    let ctx = ctx.clone();
    PRESET_CATALOG_DISPATCH.with(|slot| {
        slot.set(move |presets| {
            ctx.with_state(|ui, st| {
                st.catalog.set_presets(presets);
                refresh_catalog_ui(ui, st, true);
            });
        });
    });
    presets::watch_library(|presets| post(&PRESET_CATALOG_DISPATCH, presets));
}

fn wire_audio_preview_poll(ctx: &AppCtx) {
    let ui = ctx.ui.clone();
    // Weak: the audio preview lives inside the state, so a strong handle would leak it.
    let state_ref = Rc::downgrade(&ctx.state);
    let bridge = ctx.bridge.clone();
    ctx.state.borrow().audio_preview.set_poll_callback(move || {
        if let (Some(ui), Some(state)) = (ui.upgrade(), state_ref.upgrade()) {
            let st = &mut *state.borrow_mut();
            st.audio_preview.poll(&ui, &bridge, st.media_catalog.as_ref());
        }
    });
}

fn wire_bridge(ctx: &AppCtx) {
    {
        let ctx = ctx.clone();
        BRIDGE_DISPATCH.with(|slot| {
            slot.set(move |event: BridgeEvent| {
                if let Some(ui) = ctx.ui.upgrade() {
                    handle_bridge_event(&ui, &ctx, event);
                }
            });
        });
    }
    ctx.bridge.start_server(move |event| {
        post(&BRIDGE_DISPATCH, event);
    });
}

fn handle_bridge_event(ui: &SpotlightWindow, ctx: &AppCtx, event: BridgeEvent) {
    match event {
        BridgeEvent::ClientConnected => {
            let hotkey = ctx.state.borrow().settings.hotkey.clone();
            ui.set_bridge_connected(true);
            ui.set_status_message(SharedString::from(format!(
                "Connected to Premiere Pro · {hotkey}"
            )));
        }
        BridgeEvent::ClientDisconnected => {
            let mut st = ctx.state.borrow_mut();
            st.premiere_context = None;
            st.context_request = None;
            ui.set_context_summary("".into());
            st.results_dirty = true;
            sync_viewport_slice(ui, &mut st);
            drop(st);
            ui.set_bridge_connected(false);
            ui.set_status_message(SharedString::from(
                "Premiere disconnected · Standalone catalog active",
            ));
        }
        BridgeEvent::CatalogReceived(items) => {
            let hotkey = ctx.state.borrow().settings.hotkey.clone();
            ui.set_bridge_connected(true);
            ui.set_status_message(SharedString::from(format!(
                "Premiere synced ({} effects) · {hotkey}",
                items.len()
            )));
            let mut st = ctx.state.borrow_mut();
            st.catalog.set_catalog(items);
            refresh_catalog_ui(ui, &mut st, true);
        }
        BridgeEvent::ApplyResult(result) => {
            let mut st = ctx.state.borrow_mut();
            let completed = complete_effect(&mut st, &result);
            if !result.ok {
                let message: SharedString = if result.message.is_empty() {
                    "Premiere could not apply the effect.".into()
                } else {
                    result.message.into()
                };
                debug_log::log_event(LogKind::Error, "APPLY", message.as_str());
                ctx.toast.show(message, st.previous_hwnd);
            } else if !result.message.is_empty() {
                debug_log::log_event(LogKind::Success, "APPLY", result.message.as_str());
                ui.set_status_message(format!("Premiere: {}", result.message).into());
            }
            if let Some(open_effect_controls) = completed {
                refresh_catalog_ui(ui, &mut st, true);
                if open_effect_controls {
                    open_effect_controls_after_apply(&st, &ctx.toast);
                }
            }
            st.pending_effects
                .retain(|_, pending| Instant::now() < pending.deadline);
            schedule_action_deadlines(&st);
        }
        BridgeEvent::ProjectItemsChanged => {
            ui.set_project_loading(ctx.bridge.project_scan_pending());
            let mut st = ctx.state.borrow_mut();
            let timeline = |st: &AppState| {
                st.media_catalog
                    .as_ref()
                    .map(|media| (media.peer_id, media.project_id.clone(), media.sequence_id.clone()))
            };
            let before = timeline(&st);
            apply_project_items(ui, &mut st, &ctx.bridge, &ctx.toast);
            // Ask again when the project or sequence changed while the palette is open.
            if st.is_visible && timeline(&st) != before {
                request_premiere_context(&mut st, &ctx.bridge);
            }
        }
        BridgeEvent::ContextReceived(event) => {
            receive_premiere_context(ui, &mut ctx.state.borrow_mut(), event);
        }
        BridgeEvent::PlacementResult(result) => {
            let mut st = ctx.state.borrow_mut();
            if let Some(message) = update_placement(ui, &mut st, Some(result)) {
                debug_log::log_event(LogKind::Error, "PLACE", message.as_str());
                ctx.toast.show(message, st.previous_hwnd);
            }
            schedule_action_deadlines(&st);
        }
        BridgeEvent::SourceMonitorResult(result) => {
            let mut st = ctx.state.borrow_mut();
            if let Some(message) = update_source_open(ui, &mut st, Some(result)) {
                debug_log::log_event(LogKind::Error, "SOURCE", message.as_str());
                ctx.toast.show(message, st.previous_hwnd);
            }
            schedule_action_deadlines(&st);
        }
        BridgeEvent::ActionResult(result) => {
            let mut st = ctx.state.borrow_mut();
            finish_action(ui, &mut st, Some(result), &ctx.toast);
            schedule_action_deadlines(&st);
        }
        BridgeEvent::MarkerResult(event) => {
            receive_marker(ui, &mut ctx.state.borrow_mut(), &ctx.bridge, event, &ctx.toast);
        }
        BridgeEvent::AudioPreviewResult(result) => {
            ctx.state.borrow_mut().audio_preview.resolved(ui, result);
        }
        BridgeEvent::ShowWindowRequested => {
            let fg_hwnd = win_focus::get_foreground_hwnd();
            if win_focus::is_premiere_active_and_unminimized(fg_hwnd) {
                let mut st = ctx.state.borrow_mut();
                st.previous_hwnd = fg_hwnd;
                center_and_show(ui, &mut st, &ctx.bridge);
            }
        }
    }
}

fn open_effect_controls_after_apply(st: &AppState, toast: &Rc<ErrorToast>) {
    match premiere_prefs::read_effect_controls_shortcut() {
        ShortcutBinding::Bound(shortcut) => {
            if win_focus::open_effect_controls(st.previous_hwnd, shortcut) {
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
        ShortcutBinding::Unbound => {
            let message: SharedString =
                "Effect applied, but no shortcut is bound to Effect Controls in Premiere Pro."
                    .into();
            debug_log::log_event(LogKind::Error, "EFFECT_CONTROLS", message.as_str());
            toast.show(message, st.previous_hwnd);
        }
    }
}

fn apply_project_items(
    ui: &SpotlightWindow,
    st: &mut AppState,
    bridge: &BridgeClients,
    toast: &Rc<ErrorToast>,
) {
    let catalog = match bridge.media_catalog() {
        Ok(catalog) => catalog,
        Err(message) => {
            if st.is_visible {
                toast.show(message.into(), st.previous_hwnd);
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
        if picker.refresh_if_project_changed(bridge, catalog) {
            ui.set_marker_context("".into());
            ui.set_marker_status(picker.status().into());
            schedule_action_deadlines(st);
        }
    }
    st.media_catalog = catalog;
    if !preserve_media
        || st
            .active_bin_path
            .as_deref()
            .is_some_and(|path| !st.catalog.contains_bin_path(path))
    {
        clear_bin_scope(ui, st);
    }
    refresh_catalog_ui(ui, st, preserve_media);
}

// ---------------------------------------------------------------------------
// Palette: search, selection, activation, closing
// ---------------------------------------------------------------------------

type Activate = fn(
    usize,
    &SpotlightWindow,
    &mut AppState,
    &BridgeClients,
) -> Result<Option<PresetHandoff>, SharedString>;

/// Activates a result. `rel_idx` is a visible row for mouse clicks, or `None`
/// for the keyboard selection.
fn activate_result(ctx: &AppCtx, rel_idx: Option<i32>, activate: Activate) {
    ctx.with_state(|ui, st| {
        let abs_idx = match rel_idx {
            Some(rel_idx) => {
                let abs_idx = st.scroll_offset + (rel_idx.max(0) as usize);
                st.selected_abs_index = abs_idx;
                sync_viewport_slice(ui, st);
                sync_audio_selection(ui, st);
                abs_idx
            }
            None => st.selected_abs_index,
        };
        let result = activate(abs_idx, ui, st, &ctx.bridge);
        finish_activation(result, &ctx.toast, st.previous_hwnd);
    });
}

fn wire_palette(ui: &SpotlightWindow, ctx: &AppCtx, debug_log_ctrl: &Rc<DebugLogController>) {
    let c = ctx.clone();
    ui.on_search_edited(move |_text| {
        c.with_state(|ui, st| {
            st.selected_abs_index = 0;
            st.scroll_offset = 0;
            refresh_results_ui(ui, st);
            st.audio_preview.defer_autoplay();
        });
    });

    // Catalog tabs and the Tab key share the catalog switch.
    let c = ctx.clone();
    ui.on_catalog_changed(move || {
        c.with_state(|ui, st| {
            clear_marker_picker(ui, st);
            if current_catalog(ui) != Catalog::Project {
                clear_bin_scope(ui, st);
            }
            st.selected_abs_index = 0;
            st.scroll_offset = 0;
            refresh_results_ui(ui, st);
        });
    });

    // Step up one folder level from an active bin drill-down.
    let c = ctx.clone();
    ui.on_step_out_bin(move || c.with_state(step_out_bin));

    // Up / Down arrow selection movement
    let c = ctx.clone();
    ui.on_move_selection(move |delta| {
        c.with_state(|ui, st| {
            let total = st.matched_items.len();
            if total == 0 {
                return;
            }
            let cur = st.selected_abs_index as isize;
            st.selected_abs_index = (cur + delta as isize).rem_euclid(total as isize) as usize;
            sync_viewport_slice(ui, st);
            sync_audio_selection(ui, st);
        });
    });

    // Enter / click activates the result. Shift+Enter / Shift+Click applies
    // effects and opens Effect Controls, or opens project items in Source Monitor.
    let c = ctx.clone();
    ui.on_activate_selected(move || activate_result(&c, None, apply_entry_at));
    let c = ctx.clone();
    ui.on_activate_index(move |rel_idx| activate_result(&c, Some(rel_idx), apply_entry_at));
    let c = ctx.clone();
    ui.on_shift_activate_selected(move || activate_result(&c, None, shift_activate_at));
    let c = ctx.clone();
    ui.on_shift_activate_index(move |rel_idx| {
        activate_result(&c, Some(rel_idx), shift_activate_at)
    });

    let state_ref = ctx.state.clone();
    let debug_ctrl = debug_log_ctrl.clone();
    ui.on_toggle_debug_log(move || {
        let anchor = state_ref.borrow().previous_hwnd;
        debug_ctrl.toggle(anchor);
    });

    // Escape leaves a nested marker picker or closes the palette.
    let c = ctx.clone();
    ui.on_close_requested(move || c.with_state(escape_palette));
    let c = ctx.clone();
    ui.window().on_close_requested(move || {
        c.with_state(hide_and_restore_focus);
        slint::CloseRequestResponse::KeepWindowShown
    });
}

// ---------------------------------------------------------------------------
// Hotkeys: launch hotkey, action hotkeys, keyboard hook
// ---------------------------------------------------------------------------

const NO_HOTKEY_ID: u32 = u32::MAX;

struct HotkeyController {
    /// `None` when Windows refused to create the hotkey manager; the keyboard hook may still work.
    manager: Option<GlobalHotKeyManager>,
    registered: Option<(HotKey, String)>,
    active_id: Arc<AtomicU32>,
    available: bool,
    hook_active: bool,
}

impl HotkeyController {
    fn new(initial_spec: &str, hook_active: bool) -> (Self, Option<String>) {
        let manager = GlobalHotKeyManager::new()
            .inspect_err(|error| {
                debug_log::log_event(LogKind::Error, "HOTKEY", format!("Global hotkeys unavailable: {error}"));
            })
            .ok();
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
            if let (true, Some(manager)) = (self.available, &self.manager) {
                let _ = manager.unregister(old_hk);
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

        let Some(manager) = &self.manager else {
            return Err(format!("Windows could not set up global hotkeys, so {label} can't be used."));
        };
        let previous = self.registered.take();
        let was_available = self.available;
        if let Some((old_hk, _)) = previous {
            if was_available {
                let _ = manager.unregister(old_hk);
            }
        }

        match manager.register(hotkey) {
            Ok(()) => {
                self.active_id.store(hotkey.id(), Ordering::Release);
                self.registered = Some((hotkey, label));
                self.available = true;
                Ok(())
            }
            Err(error) => {
                if let Some((old_hk, old_label)) = previous {
                    if was_available && manager.register(old_hk).is_ok() {
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

/// Runs a launch-hotkey press on the UI thread. Hook threads set `pending`
/// first; this consumes it so a press is handled once.
fn wire_launch_dispatch(ctx: &AppCtx, pending: &Arc<AtomicBool>) {
    let ctx = ctx.clone();
    let pending_flag = pending.clone();
    HOTKEY_DISPATCH.with(|slot| {
        slot.set(move |()| {
            if !pending_flag.swap(false, Ordering::AcqRel) {
                return;
            }
            ctx.with_state(|ui, st| toggle_palette_from_hotkey(ui, st, &ctx.bridge));
        });
    });
}

fn install_action_hotkey_hook(
    ui: &SpotlightWindow,
    ctx: &AppCtx,
    launch_pending: &Arc<AtomicBool>,
) -> Option<action_hotkeys::ActionHotkeyHook> {
    {
        let ctx = ctx.clone();
        ACTION_HOTKEY_DISPATCH.with(|slot| {
            slot.set(move |(binding, hwnd): (action_hotkeys::Binding, isize)| {
                ctx.with_state(|ui, state| {
                    if binding.is_current(&state.settings) {
                        dispatch_action_hotkey(
                            binding.action, ui, state, &ctx.bridge, &ctx.toast, Some(hwnd),
                        );
                    }
                });
            });
        });
    }
    let rec_flag = ctx.recording.clone();
    let rec_flag_launch = ctx.recording.clone();
    let launch_pending = launch_pending.clone();
    match action_hotkeys::ActionHotkeyHook::install(
        move |binding, hwnd| {
            !rec_flag.load(Ordering::Acquire) && post(&ACTION_HOTKEY_DISPATCH, (binding, hwnd))
        },
        move || {
            if rec_flag_launch.load(Ordering::Acquire) {
                return false;
            }
            launch_pending.store(true, Ordering::Release);
            post(&HOTKEY_DISPATCH, ())
        },
    ) {
        Ok(hook) => Some(hook),
        Err(message) => {
            ui.set_action_hotkeys_warning(message.into());
            None
        }
    }
}

fn create_hotkey_controller(
    ui: &SpotlightWindow,
    ctx: &AppCtx,
    initial_spec: &str,
    hook_active: bool,
) -> Rc<RefCell<HotkeyController>> {
    let (hotkey_ctrl, initial_hotkey_err) = HotkeyController::new(initial_spec, hook_active);
    let st = ctx.state.borrow();
    sync_settings_ui(ui, &st.settings);
    if let Some(err) = initial_hotkey_err {
        ui.set_settings_feedback_error(true);
        ui.set_settings_feedback(err.into());
    }
    Rc::new(RefCell::new(hotkey_ctrl))
}

/// Handles the launch hotkey when Windows delivers it as a registered hotkey
/// (used when the keyboard hook could not take it).
fn wire_registered_hotkey_events(
    ctx: &AppCtx,
    hotkey_id: Arc<AtomicU32>,
    pending_flag: Arc<AtomicBool>,
) {
    let rec_flag = ctx.recording.clone();
    let ui_weak = ctx.ui.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
        if event.state != HotKeyState::Pressed || event.id != hotkey_id.load(Ordering::Acquire) {
            return;
        }
        if rec_flag.load(Ordering::Acquire) {
            // Windows delivers the registered launch chord here instead of to the recorder.
            let _ = ui_weak.upgrade_in_event_loop(|ui| {
                if ui.get_recording_hotkey() && ui.get_recording_target() != settings::LAUNCH_SHORTCUT_ID {
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
        post(&HOTKEY_DISPATCH, ());
    }));
}

// ---------------------------------------------------------------------------
// Settings view
// ---------------------------------------------------------------------------

fn modifier_label(modifier: AlternateModifier) -> &'static str {
    match modifier {
        AlternateModifier::Shift => "Shift",
        AlternateModifier::Ctrl => "Ctrl",
        AlternateModifier::Alt => "Alt",
    }
}

fn sync_settings_ui(ui: &SpotlightWindow, settings: &AppSettings) {
    apply_palette_theme(ui, settings.theme);
    ui.set_hotkey_label(settings.hotkey.clone().into());
    ui.set_open_effect_controls_default(settings.open_effect_controls_by_default);
    ui.set_alternate_modifier(settings.alternate_modifier.as_str().into());
    for slot in PaletteShortcut::ALL {
        let label = SharedString::from(slot.get(settings).as_str());
        match slot {
            PaletteShortcut::Settings => ui.set_settings_hotkey_label(label),
            PaletteShortcut::CatalogAll => ui.set_catalog_all_hotkey_label(label),
            PaletteShortcut::CatalogEffects => ui.set_catalog_effects_hotkey_label(label),
            PaletteShortcut::CatalogProject => ui.set_catalog_project_hotkey_label(label),
            PaletteShortcut::CatalogActions => ui.set_catalog_actions_hotkey_label(label),
            PaletteShortcut::AudioPreview => ui.set_audio_preview_hotkey_label(label),
        }
    }
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

fn show_settings_error(ui: &SpotlightWindow, message: impl Into<SharedString>) {
    ui.set_settings_feedback_error(true);
    ui.set_settings_feedback(message.into());
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
            show_settings_error(ui, err.clone());
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
    if let Some(conflict) = settings::shortcut_conflict(settings::LAUNCH_SHORTCUT_ID, &label, &state.settings) {
        show_settings_error(ui, conflict.clone());
        sync_settings_ui(ui, &state.settings);
        return Err(conflict);
    }
    let result = hotkey_ctrl.borrow_mut().apply(hotkey, label.clone());
    match result {
        Ok(()) => {
            state.settings.hotkey = label.clone();
            save_and_sync_settings(ui, &state.settings, format!("Saved shortcut: {label}"))
        }
        Err(err) => {
            show_settings_error(ui, err.clone());
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

fn toggle_default_effect_controls_setting(ui: &SpotlightWindow, state: &mut AppState) {
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

fn toggle_auto_preview_audio(ui: &SpotlightWindow, state: &mut AppState) {
    state.settings.auto_preview_audio = !state.settings.auto_preview_audio;
    let message = if state.settings.auto_preview_audio {
        "Auto-preview audio enabled."
    } else {
        "Auto-preview audio disabled."
    };
    let _ = save_and_sync_settings(ui, &state.settings, message.into());
}

fn set_alternate_modifier(ui: &SpotlightWindow, state: &mut AppState, next: AlternateModifier) {
    if state.settings.alternate_modifier == next {
        return;
    }
    state.settings.alternate_modifier = next;
    let _ = save_and_sync_settings(
        ui,
        &state.settings,
        format!("Alternate action modifier set to {}.", modifier_label(next)),
    );
}

fn reload_settings_live(
    ui: &SpotlightWindow,
    state: &mut AppState,
    hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
) {
    let (mut loaded, mut feedback, file_warning) = settings::load_settings_with_feedback();
    if file_warning.is_some() {
        // Keep the working settings instead of swapping in defaults from an unreadable file.
        show_settings_error(
            ui,
            "Couldn't reload: the settings file has an error. Your current settings are unchanged.",
        );
        return;
    }
    // Every setting comes straight from the file except two: the launch hotkey
    // stays until the new one registers, and action hotkeys are re-validated below.
    let loaded_hotkey = std::mem::replace(&mut loaded.hotkey, std::mem::take(&mut state.settings.hotkey));
    let loaded_action_hotkeys = std::mem::take(&mut loaded.action_hotkeys);
    state.settings = loaded;

    let mut error_msg = None;
    if let Ok((hotkey, label)) = settings::parse_hotkey_spec(&loaded_hotkey) {
        if let Some(conflict) = settings::shortcut_conflict(settings::LAUNCH_SHORTCUT_ID, &label, &state.settings) {
            error_msg = Some(conflict);
        } else if let Err(err) = hotkey_ctrl.borrow_mut().apply(hotkey, label.clone()) {
            error_msg = Some(err);
        } else {
            state.settings.hotkey = label;
        }
    }
    if let Some(err) = error_msg {
        let audio_preview = PaletteShortcut::AudioPreview;
        if settings::shortcut_conflict(audio_preview.id(), audio_preview.get(&state.settings), &state.settings).is_some() {
            audio_preview.get_mut(&mut state.settings).clear();
        }
        feedback.push(err);
    }
    state.settings.action_hotkeys = loaded_action_hotkeys;
    feedback.extend(settings::normalize_action_hotkeys(&mut state.settings));
    if feedback.is_empty() {
        ui.set_settings_feedback_error(false);
        ui.set_settings_feedback("Reloaded settings from config.json".into());
    } else {
        show_settings_error(ui, feedback.join(" "));
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

fn jump_to_catalog(ui: &SpotlightWindow, state: &mut AppState, target: Catalog) {
    if current_catalog(ui) == target {
        return;
    }
    clear_marker_picker(ui, state);
    set_catalog(ui, target);
    if target != Catalog::Project {
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
    // JSON may have no default app; Notepad always exists on Windows.
    let opened = win_focus::open_with_default_app(&path)
        || (cfg!(windows) && std::process::Command::new("notepad.exe").arg(&path).spawn().is_ok());
    if !opened {
        debug_log::log_event(
            LogKind::Error,
            "SETTINGS",
            format!("Could not open {}", path.display()),
        );
    }
}

/// Saves a shortcut recorded for the settings view. `launch` goes through
/// the registered hotkey; everything else is a palette or action shortcut.
fn capture_shortcut(
    ctx: &AppCtx,
    hotkey_ctrl: &Rc<RefCell<HotkeyController>>,
    ui: &SpotlightWindow,
    target: &str,
    event: (&str, bool, bool, bool, bool),
) {
    let (text, ctrl, alt, shift, meta) = event;
    if target == settings::LAUNCH_SHORTCUT_ID {
        match settings::hotkey_from_slint_event(text, ctrl, alt, shift, meta) {
            Ok(None) => {}
            Ok(Some((hotkey, label))) => {
                stop_shortcut_recording(ui, Some(&ctx.recording));
                let mut st = ctx.state.borrow_mut();
                let _ = apply_hotkey_update(ui, &mut st, hotkey_ctrl, hotkey, label);
            }
            Err(msg) => show_settings_error(ui, msg),
        }
        return;
    }
    match settings::palette_hotkey_from_slint_event(text, ctrl, alt, shift, meta) {
        Ok(None) => {}
        Ok(Some((_hotkey, label))) => {
            stop_shortcut_recording(ui, Some(&ctx.recording));
            let mut st = ctx.state.borrow_mut();
            if let Some(action) = Action::from_id(target) {
                update_action_hotkey(ui, &mut st, action, Some(&label));
                return;
            }
            if let Some(conflict) = settings::shortcut_conflict(target, &label, &st.settings) {
                show_settings_error(ui, conflict);
                return;
            }
            if let Some(slot) = PaletteShortcut::from_id(target) {
                *slot.get_mut(&mut st.settings) = label.clone();
                let _ = save_and_sync_settings(ui, &st.settings, format!("Saved shortcut: {label}"));
            }
        }
        Err(msg) => show_settings_error(ui, msg),
    }
}

fn clear_shortcut(ctx: &AppCtx, ui: &SpotlightWindow, target: &str) {
    stop_shortcut_recording(ui, Some(&ctx.recording));
    let mut st = ctx.state.borrow_mut();
    if let Some(action) = Action::from_id(target) {
        update_action_hotkey(ui, &mut st, action, None);
        return;
    }
    if let Some(slot) = PaletteShortcut::from_id(target) {
        slot.get_mut(&mut st.settings).clear();
        let _ = save_and_sync_settings(ui, &st.settings, format!("Cleared {}.", slot.field_name()));
    }
}

/// Runs a palette shortcut typed while the palette has focus. Returns
/// whether the key was handled.
fn try_palette_shortcut(
    ctx: &AppCtx,
    ui: &SpotlightWindow,
    event: (&str, bool, bool, bool, bool),
    repeat: bool,
) -> bool {
    let (text, ctrl, alt, shift, meta) = event;
    let settings_open = ui.get_settings_open();
    let action = {
        let st = ctx.state.borrow();
        settings::match_palette_shortcut(&st.settings, settings_open, text, ctrl, alt, shift, meta)
    };
    match action {
        Some(PaletteShortcutAction::ToggleSettings) => {
            toggle_settings_view(ui, &mut ctx.state.borrow_mut(), &ctx.recording);
            true
        }
        Some(PaletteShortcutAction::SelectCatalog(target)) => {
            if settings_open {
                stop_shortcut_recording(ui, Some(&ctx.recording));
                ui.set_settings_feedback("".into());
                ui.set_settings_open(false);
                ui.invoke_focus_search();
            }
            let mut st = ctx.state.borrow_mut();
            jump_to_catalog(ui, &mut st, target);
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
                    action, ui, &mut ctx.state.borrow_mut(), &ctx.bridge, &ctx.toast, None,
                );
            }
            true
        }
        None => false,
    }
}

fn wire_settings(ui: &SpotlightWindow, ctx: &AppCtx, hotkey_ctrl: &Rc<RefCell<HotkeyController>>) {
    let c = ctx.clone();
    ui.on_select_theme(move |label| {
        c.with_state(|ui, st| {
            change_palette_theme(ui, &mut st.settings, label.as_str(), &settings::config_path());
        });
    });

    let c = ctx.clone();
    ui.on_toggle_settings(move || {
        c.with_state(|ui, st| toggle_settings_view(ui, st, &c.recording));
    });

    let rec_flag = ctx.recording.clone();
    ui.on_hotkey_recording_changed(move |active| {
        rec_flag.store(active, Ordering::Release);
    });

    let c = ctx.clone();
    let hk_ctrl = hotkey_ctrl.clone();
    ui.on_capture_shortcut(move |target, text, ctrl, alt, shift, meta| {
        if let Some(ui) = c.ui.upgrade() {
            capture_shortcut(&c, &hk_ctrl, &ui, target.as_str(), (text.as_str(), ctrl, alt, shift, meta));
        }
    });

    let c = ctx.clone();
    ui.on_clear_shortcut(move |target| {
        if let Some(ui) = c.ui.upgrade() {
            clear_shortcut(&c, &ui, target.as_str());
        }
    });

    let c = ctx.clone();
    let hk_ctrl = hotkey_ctrl.clone();
    ui.on_reset_default_hotkey(move || {
        let Some(ui) = c.ui.upgrade() else {
            return;
        };
        stop_shortcut_recording(&ui, Some(&c.recording));
        if let Ok((hotkey, label)) = settings::parse_hotkey_spec(settings::DEFAULT_HOTKEY) {
            let mut st = c.state.borrow_mut();
            let _ = apply_hotkey_update(&ui, &mut st, &hk_ctrl, hotkey, label);
        }
    });

    let c = ctx.clone();
    ui.on_set_alternate_modifier(move |value| {
        let next = AlternateModifier::from_setting(value.as_str());
        c.with_state(|ui, st| set_alternate_modifier(ui, st, next));
    });

    let c = ctx.clone();
    ui.on_try_palette_shortcut(move |text, ctrl, alt, shift, meta, repeat| {
        let Some(ui) = c.ui.upgrade() else {
            return false;
        };
        try_palette_shortcut(&c, &ui, (text.as_str(), ctrl, alt, shift, meta), repeat)
    });

    let c = ctx.clone();
    ui.on_toggle_preview_playback(move || {
        c.with_state(|ui, st| {
            if st.is_visible && !ui.get_settings_open() {
                st.audio_preview.toggle(ui, &c.bridge, st.media_catalog.as_ref());
            }
        });
    });

    let c = ctx.clone();
    ui.on_seek_audio_preview(move |fraction| {
        c.with_state(|ui, st| st.audio_preview.seek(ui, fraction));
    });

    let c = ctx.clone();
    ui.on_toggle_auto_preview_audio(move || c.with_state(toggle_auto_preview_audio));

    let c = ctx.clone();
    ui.on_toggle_default_effect_controls(move || {
        c.with_state(toggle_default_effect_controls_setting)
    });

    let state_ref = ctx.state.clone();
    ui.on_open_config_file(move || open_config_file_in_editor(&state_ref.borrow()));

    let c = ctx.clone();
    let hk_ctrl = hotkey_ctrl.clone();
    ui.on_reload_settings(move || {
        c.with_state(|ui, st| reload_settings_live(ui, st, &hk_ctrl));
    });
}

// ---------------------------------------------------------------------------
// Focus tracking, quitting, tray, deadlines
// ---------------------------------------------------------------------------

/// Hides the palette when it loses focus. The returned hook must live until
/// the event loop ends.
fn wire_focus_tracking(ctx: &AppCtx) -> win_focus::ForegroundHook {
    let hook_active = Rc::new(Cell::new(false));
    {
        let ctx = ctx.clone();
        let hook_active = hook_active.clone();
        SYNC_FOCUS_DISPATCH.with(|slot| {
            slot.set(move |()| {
                if let Some(ui) = ctx.ui.upgrade() {
                    sync_palette_focus(&ui, &ctx, hook_active.get());
                }
            });
        });
    }
    // Features that react to Premiere losing the foreground or minimizing.
    #[cfg(windows)]
    {
        win_focus::on_foreground_change(|hwnd, _| presets::foreground_changed(hwnd));
        win_focus::on_foreground_change(crate::actions::markers::foreground_changed);
    }
    win_focus::on_foreground_change(|hwnd, _| action_hotkeys::foreground_changed(hwnd));
    win_focus::on_premiere_minimize(action_hotkeys::release_active_hold);
    let foreground_hook = win_focus::ForegroundHook::install(|| {
        post(&SYNC_FOCUS_DISPATCH, ());
    });
    hook_active.set(foreground_hook.is_active());
    foreground_hook
}

fn sync_palette_focus(ui: &SpotlightWindow, ctx: &AppCtx, hook_active: bool) {
    if !ctx.state.borrow().is_visible {
        stop_focus_grace_timer();
        return;
    }
    let our_hwnd = extract_window_hwnd(ui);
    win_focus::init_palette_window(our_hwnd);
    let is_our_window = win_focus::is_palette_open_and_active(our_hwnd);

    let (focus_outcome, saw_foreground) = {
        let mut st = ctx.state.borrow_mut();
        let outcome = evaluate_palette_focus(&mut st, is_our_window, Instant::now());
        (outcome, st.saw_foreground)
    };
    match focus_outcome {
        FocusOutcome::KeepVisible => {
            // Once Windows confirms foreground and WinEventHook is active, stop the 20ms grace timer.
            if saw_foreground && hook_active {
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
            stop_shortcut_recording(ui, Some(&ctx.recording));
            hide_palette(ui, &mut ctx.state.borrow_mut());
        }
    }
}

fn make_quit_app(ctx: &AppCtx, tray: Rc<RefCell<Option<SystemTray>>>) -> Rc<dyn Fn()> {
    let ctx = ctx.clone();
    Rc::new(move || {
        ctx.with_state(hide_palette);
        if let Some(ref mut t) = *tray.borrow_mut() {
            t.remove();
        }
        let _ = slint::quit_event_loop();
    })
}

fn wire_premiere_exit(quit_app: Rc<dyn Fn()>) {
    PREMIERE_EXIT_DISPATCH.with(|slot| {
        slot.set(move |()| quit_app());
    });
    win_focus::spawn_premiere_exit_watcher(|| {
        // Quit once, even if the watcher reports the exit again.
        let _ = slint::invoke_from_event_loop(|| PREMIERE_EXIT_DISPATCH.with(|slot| slot.call_once(())));
    });
}

fn wire_tray(ctx: &AppCtx, quit_app: Rc<dyn Fn()>) {
    let ctx = ctx.clone();
    tray::set_action_handler(move || {
        let Some(ui) = ctx.ui.upgrade() else {
            return;
        };
        while let Some(action) = tray::poll_tray_action() {
            match action {
                TrayAction::OpenSpotlight => {
                    let (_, unminimized_premiere_hwnd) = win_focus::query_premiere_window_state();
                    if unminimized_premiere_hwnd != 0 {
                        let mut st = ctx.state.borrow_mut();
                        st.previous_hwnd = unminimized_premiere_hwnd;
                        center_and_show(&ui, &mut st, &ctx.bridge);
                    }
                }
                TrayAction::OpenSettings => {
                    let (_, unminimized_premiere_hwnd) = win_focus::query_premiere_window_state();
                    let mut st = ctx.state.borrow_mut();
                    st.previous_hwnd = unminimized_premiere_hwnd;
                    stop_shortcut_recording(&ui, Some(&ctx.recording));
                    ui.set_settings_feedback("".into());
                    ui.set_settings_open(true);
                    show_palette(&ui, &mut st);
                }
                TrayAction::Quit => quit_app(),
            }
        }
    });
}

/// Expires effects, placements, Source Monitor opens, actions and marker
/// requests that Premiere never answered.
fn wire_deadlines(ctx: &AppCtx) {
    let ctx = ctx.clone();
    DEADLINE_DISPATCH.with(|slot| {
        slot.set(move |()| {
            ctx.with_state(|ui, st| {
                st.pending_effects
                    .retain(|_, pending| Instant::now() < pending.deadline);
                if let Some(message) = update_placement(ui, st, None) {
                    debug_log::log_event(LogKind::Error, "PLACE", message.as_str());
                    ctx.toast.show(message, st.previous_hwnd);
                }
                if let Some(message) = update_source_open(ui, st, None) {
                    debug_log::log_event(LogKind::Error, "SOURCE", message.as_str());
                    ctx.toast.show(message, st.previous_hwnd);
                }
                finish_action(ui, st, None, &ctx.toast);
                poll_markers(ui, st, &ctx.bridge, &ctx.toast);
                schedule_action_deadlines(st);
            });
        });
    });
}
