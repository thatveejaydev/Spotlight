//! The palette app: shared state, timers and the event-loop dispatch slots.
//! Submodules hold the result list, the palette window, Premiere replies and
//! activation; [`startup`] wires them to the window, bridge and hooks.

mod activation;
mod palette;
mod requests;
mod results;
mod startup;
#[cfg(test)]
mod tests;

pub(crate) use startup::run;

use activation::*;
use palette::*;
use requests::*;
use results::*;

use crate::{
    action_hotkeys,
    actions::{
        markers::{MarkerDialog, MarkerEvent, MarkerPicker},
        Action, ActionResult, PendingAction, ACTION_TIMEOUT,
    },
    audio_preview::AudioPreview,
    bridge::{
        self, ApplyResult, BridgeClients, BridgeEvent, EffectControlsShortcut, MediaCatalog,
        PlacementResult,
    },
    catalog::{Catalog, CatalogStore, EffectEntry, ProjectItemEntry, ProjectItemKind, SearchResult},
    debug_log::{self, DebugLogController, LogKind},
    error_toast::{self, ErrorToast},
    presets::{self, PresetHandoff},
    settings::{
        self, AlternateModifier, AppSettings, PaletteShortcut, PaletteShortcutAction, PaletteTheme,
    },
    tray::{self, SystemTray, TrayAction},
    win_focus, ActionHotkeyUI, EffectItemUI, SpotlightWindow,
};
#[cfg(test)]
use crate::{actions, catalog};
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
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

const VISIBLE_ROWS: usize = 8;
const MAX_RECENT_ITEMS: usize = 20;
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
