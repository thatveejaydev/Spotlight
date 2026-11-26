//! The palette app: shared state, timers and the event-loop dispatch slots.
//! Submodules hold the result list, the palette window, Premiere replies and
//! activation; [`startup`] wires them to the window, bridge and hooks.

mod activation;
mod dispatch;
mod palette;
mod requests;
mod results;
mod startup;
#[cfg(test)]
mod tests;

pub(crate) use startup::run;

use activation::*;
use dispatch::{post, UiSlot};
use palette::*;
use requests::*;
use results::*;

use crate::{
    action_hotkeys,
    actions::{
        markers::{MarkerDialog, MarkerPicker},
        Action,
    },
    audio_preview::{AudioPreview, PreviewItem},
    availability::{self, Availability, Facts},
    bridge::{ApplyResult, BridgeClients, BridgeEvent, ContextEvent, MarkerEvent, MediaCatalog},
    catalog::{Catalog, CatalogStore, EffectEntry, EffectType, MediaType, ProjectItemEntry, ProjectItemKind, SearchResult},
    debug_log::{self, DebugLogController, LogKind},
    error_toast::{self, ErrorToast},
    library::{self, FolderStatus, LibraryIndexer, LibrarySnapshot},
    premiere_prefs::{self, ShortcutBinding},
    presets::{self, PresetHandoff},
    protocol::{MarkerCommand, MarkerReply, PremiereContext},
    request::{CommandReply, PendingRequest},
    settings::{
        self, AlternateModifier, AppSettings, PaletteShortcut, PaletteShortcutAction, PaletteTheme,
    },
    tray::{self, SystemTray, TrayAction},
    win_focus, ActionHotkeyUI, EffectItemUI, LibraryFolderUI, SpotlightWindow,
};
#[cfg(test)]
use crate::{bridge, catalog};
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
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
    /// What Premiere reported when the palette opened; `None` until it answers.
    premiere_context: Option<PremiereContext>,
    context_request: Option<PendingRequest>,
    pending_placement: Option<PendingPlacement>,
    pending_open: Option<PendingOpen>,
    pending_action: Option<PendingRequest>,
    marker_picker: Option<MarkerPicker>,
    marker_dialog: Option<MarkerDialog>,
    /// Set while the search box is the name field of Add Named Marker.
    marker_naming: Option<MarkerNaming>,
    pending_named_marker: Option<PendingNamedMarker>,
    audio_preview: AudioPreview,
    /// What the indexer last found in each tracked Library folder.
    library_status: Vec<FolderStatus>,
    /// True while Windows' folder picker is open over the palette.
    folder_picker_open: bool,
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

impl AppState {
    fn new(settings: AppSettings) -> Self {
        Self {
            catalog: CatalogStore::new_with_defaults(),
            matched_items: Vec::new(),
            recent_items: Vec::new(),
            pending_effects: HashMap::new(),
            media_catalog: None,
            premiere_context: None,
            context_request: None,
            pending_placement: None,
            pending_open: None,
            pending_action: None,
            marker_picker: None,
            marker_dialog: None,
            marker_naming: None,
            pending_named_marker: None,
            audio_preview: AudioPreview::default(),
            library_status: Vec::new(),
            folder_picker_open: false,
            active_bin_path: None,
            settings,
            selected_abs_index: 0,
            scroll_offset: 0,
            rendered_scroll_offset: None,
            results_dirty: true,
            is_visible: false,
            shown_at: None,
            saw_foreground: false,
            previous_hwnd: 0,
        }
    }
}

impl AppState {
    fn facts(&self) -> Facts {
        Facts::new(self.premiere_context.as_ref(), self.media_catalog.as_ref())
    }
}

struct PendingEffect {
    entry: Arc<EffectEntry>,
    request: PendingRequest,
    open_effect_controls: bool,
}

struct PendingPlacement {
    request: PendingRequest,
    /// Added to the recent items once Premiere confirms the placement.
    recent: RecentItem,
}

struct RecentItem {
    item: SearchResult,
    project_id: Option<String>,
}

/// Where the alternate action opens a project item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenTarget {
    /// Media opens in the Source Monitor.
    SourceMonitor,
    /// Sequences open in their own Timeline tab.
    Timeline,
}

impl OpenTarget {
    fn for_item(entry: &ProjectItemEntry) -> Self {
        match entry.kind {
            ProjectItemKind::Sequence | ProjectItemKind::ActiveSequence => Self::Timeline,
            _ => Self::SourceMonitor,
        }
    }

    fn failure(self) -> &'static str {
        match self {
            Self::SourceMonitor => "Premiere could not open the item in the Source Monitor.",
            Self::Timeline => "Premiere could not open the sequence in the Timeline.",
        }
    }

    fn timeout(self) -> &'static str {
        match self {
            Self::SourceMonitor => "Source Monitor didn't respond. Check Premiere.",
            Self::Timeline => "Premiere didn't confirm opening the sequence. Check the Timeline.",
        }
    }
}

struct MarkerNaming {
    /// Restored when Esc leaves the name field.
    previous_query: SharedString,
}

struct PendingNamedMarker {
    request: PendingRequest,
    name: String,
}

struct PendingOpen {
    request: PendingRequest,
    target: OpenTarget,
}

/// How long a result message replaces the footer's key hints.
const STATUS_DURATION: Duration = Duration::from_secs(4);

/// Shows `message` in the footer for a few seconds, then brings the key hints back.
pub(crate) fn show_status(ui: &SpotlightWindow, message: impl Into<SharedString>) {
    ui.set_status_message(message.into());
    ui.set_status_visible(true);
    let weak = ui.as_weak();
    STATUS_TIMER.with(|slot| {
        let mut guard = slot.borrow_mut();
        let timer = guard.get_or_insert_with(Timer::default);
        timer.start(TimerMode::SingleShot, STATUS_DURATION, move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_status_visible(false);
            }
        });
    });
}

fn start_focus_grace_timer() {
    FOCUS_TIMER.with(|slot| {
        let mut guard = slot.borrow_mut();
        let timer = guard.get_or_insert_with(Timer::default);
        timer.start(TimerMode::Repeated, Duration::from_millis(20), || {
            debug_log::record_ui_timer_tick();
            SYNC_FOCUS_DISPATCH.with(|slot| slot.call(()));
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
        state.pending_placement.as_ref().map(|p| p.request.deadline),
        state.pending_open.as_ref().map(|p| p.request.deadline),
        state.pending_action.as_ref().map(|p| p.deadline),
        state.marker_picker.as_ref().and_then(MarkerPicker::deadline),
        state.marker_dialog.as_ref().map(MarkerDialog::deadline),
        state.pending_named_marker.as_ref().map(|p| p.request.deadline),
    ]
    .into_iter()
    .flatten()
    .chain(state.pending_effects.values().map(|p| p.request.deadline))
    .min();

    DEADLINE_TIMER.with(|slot| {
        if let Some(deadline) = earliest {
            let delay = deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(10);
            let mut guard = slot.borrow_mut();
            let timer = guard.get_or_insert_with(Timer::default);
            timer.start(TimerMode::SingleShot, delay, || {
                DEADLINE_DISPATCH.with(|slot| slot.call(()));
            });
        } else if let Some(timer) = slot.borrow().as_ref() {
            timer.stop();
        }
    });
}

thread_local! {
    static BRIDGE_DISPATCH: UiSlot<BridgeEvent> = const { UiSlot::new() };
    static PRESET_CATALOG_DISPATCH: UiSlot<Vec<EffectEntry>> = const { UiSlot::new() };
    static LIBRARY_DISPATCH: UiSlot<LibrarySnapshot> = const { UiSlot::new() };
    static FOLDER_PICK_DISPATCH: UiSlot<Option<Vec<PathBuf>>> = const { UiSlot::new() };
    static HOTKEY_DISPATCH: UiSlot<()> = const { UiSlot::new() };
    static ACTION_HOTKEY_DISPATCH: UiSlot<(action_hotkeys::Binding, isize)> = const { UiSlot::new() };
    static SYNC_FOCUS_DISPATCH: UiSlot<()> = const { UiSlot::new() };
    static DEADLINE_DISPATCH: UiSlot<()> = const { UiSlot::new() };
    static PREMIERE_EXIT_DISPATCH: UiSlot<()> = const { UiSlot::new() };
    static FOCUS_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    static DEADLINE_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
    static STATUS_TIMER: RefCell<Option<Timer>> = const { RefCell::new(None) };
}
