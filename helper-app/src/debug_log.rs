use crate::{win_focus, DebugLogEntryUI, DebugLogWindow};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
thread_local! {
    pub(crate) static CREATING_DEBUG_WINDOW: Cell<bool> = const { Cell::new(false) };
}

const MAX_LOG_ENTRIES: usize = 150;

static UI_TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static BRIDGE_TIMEOUT_WAKEUPS: AtomicU64 = AtomicU64::new(0);
static FG_HOOK_EVENTS: AtomicU64 = AtomicU64::new(0);
static HAS_NEW_LOGS: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Bridge,
    Search,
    Focus,
    Success,
    Error,
}

#[derive(Clone)]
struct StoredEntry {
    kind: LogKind,
    text: String,
}

struct SharedMetrics {
    entries: VecDeque<StoredEntry>,
    last_search: String,
    last_bridge_rtt: String,
}

impl Default for SharedMetrics {
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(MAX_LOG_ENTRIES),
            last_search: "—".to_string(),
            last_bridge_rtt: "—".to_string(),
        }
    }
}

fn shared() -> &'static Mutex<SharedMetrics> {
    static INSTANCE: OnceLock<Mutex<SharedMetrics>> = OnceLock::new();
    INSTANCE.get_or_init(|| Mutex::new(SharedMetrics::default()))
}

fn format_timestamp() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = dur.as_secs() % 86_400;
    let hours = total_secs / 3_600;
    let mins = (total_secs % 3_600) / 60;
    let secs = total_secs % 60;
    let millis = dur.subsec_millis();
    format!("{hours:02}:{mins:02}:{secs:02}.{millis:03}Z")
}

fn kind_color(kind: LogKind) -> Color {
    match kind {
        LogKind::Info => Color::from_rgb_u8(203, 213, 225),
        LogKind::Bridge => Color::from_rgb_u8(125, 211, 252),
        LogKind::Search => Color::from_rgb_u8(196, 181, 253),
        LogKind::Focus => Color::from_rgb_u8(252, 211, 77),
        LogKind::Success => Color::from_rgb_u8(110, 231, 183),
        LogKind::Error => Color::from_rgb_u8(252, 165, 165),
    }
}

/// Appends log lines to the file named by `PREMIERE_SPOTLIGHT_LOG`, when set (diagnostics only).
fn append_log_file(line: &str) {
    static PATH: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
    let Some(path) = PATH.get_or_init(|| std::env::var_os("PREMIERE_SPOTLIGHT_LOG").map(Into::into)) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

/// Release builds use `panic = "abort"`, so a panic closes the app at once.
/// This hook appends the panic message and location to `crash.log` next to
/// the config file first, so the crash leaves a trace.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let line = format!(
            "[{}] [PANIC] thread '{}': {}",
            format_timestamp(),
            thread.name().unwrap_or("<unnamed>"),
            info
        );
        append_log_file(&line);
        if let Some(dir) = crate::settings::config_path().parent() {
            use std::io::Write;
            let _ = std::fs::create_dir_all(dir);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("crash.log"))
            {
                let _ = writeln!(file, "{line}");
            }
        }
        default_hook(info);
    }));
}

pub fn record_ui_timer_tick() {
    UI_TIMER_TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_bridge_timeout() {
    BRIDGE_TIMEOUT_WAKEUPS.fetch_add(1, Ordering::Relaxed);
}

pub fn record_fg_hook_event() {
    FG_HOOK_EVENTS.fetch_add(1, Ordering::Relaxed);
}

pub fn log_event(kind: LogKind, tag: &str, message: impl AsRef<str>) {
    let line = format!("[{}] [{}] {}", format_timestamp(), tag, message.as_ref());
    append_log_file(&line);
    if let Ok(mut guard) = shared().lock() {
        if guard.entries.len() >= MAX_LOG_ENTRIES {
            guard.entries.pop_back();
        }
        guard.entries.push_front(StoredEntry { kind, text: line });
        HAS_NEW_LOGS.store(true, Ordering::Release);
    }
}

pub fn record_search_timing(
    duration: Duration,
    query: &str,
    catalog: &str,
    matches: usize,
    total_catalog_items: usize,
) {
    let ms = duration.as_secs_f64() * 1000.0;
    let summary = format!("{ms:.2} ms ({matches}/{total_catalog_items})");
    if let Ok(mut guard) = shared().lock() {
        guard.last_search = summary;
    }
    let display_query = if query.is_empty() { "<empty>" } else { query };
    log_event(
        LogKind::Search,
        "SEARCH",
        format!("\"{display_query}\" [{catalog}] -> {matches} matches in {ms:.2} ms"),
    );
}

pub fn record_bridge_rtt(label: &str, duration: Duration) {
    let ms = duration.as_secs_f64() * 1000.0;
    let summary = format!("{ms:.1} ms ({label})");
    if let Ok(mut guard) = shared().lock() {
        guard.last_bridge_rtt = summary;
    }
    log_event(
        LogKind::Bridge,
        "RTT",
        format!("{label} round-trip in {ms:.1} ms"),
    );
}

pub fn clear_logs() {
    if let Ok(mut guard) = shared().lock() {
        guard.entries.clear();
        HAS_NEW_LOGS.store(true, Ordering::Release);
    }
}

struct SampleSnapshot {
    at: Instant,
    ui_ticks: u64,
    ws_polls: u64,
    fg_hooks: u64,
}

impl SampleSnapshot {
    fn capture() -> Self {
        Self {
            at: Instant::now(),
            ui_ticks: UI_TIMER_TICKS.load(Ordering::Relaxed),
            ws_polls: BRIDGE_TIMEOUT_WAKEUPS.load(Ordering::Relaxed),
            fg_hooks: FG_HOOK_EVENTS.load(Ordering::Relaxed),
        }
    }
}

#[derive(Default)]
pub(crate) struct DebugLogController {
    window: RefCell<Option<DebugLogWindow>>,
    refresh_timer: Timer,
    paused: Cell<bool>,
    last_sample: RefCell<Option<SampleSnapshot>>,
}

impl DebugLogController {
    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.window.borrow().is_some()
    }

    pub(crate) fn toggle(self: &Rc<Self>, anchor_hwnd: isize) {
        if self.window.borrow().is_some() {
            self.close();
        } else {
            self.open(anchor_hwnd);
        }
    }

    pub(crate) fn close(&self) {
        self.refresh_timer.stop();
        *self.last_sample.borrow_mut() = None;
        if let Some(window) = self.window.borrow_mut().take() {
            let _ = window.hide();
        }
    }

    fn open(self: &Rc<Self>, anchor_hwnd: isize) {
        #[cfg(windows)]
        CREATING_DEBUG_WINDOW.with(|flag| flag.set(true));

        let created = (|| {
            let window = DebugLogWindow::new()?;
            if let Some(pos) = win_focus::debug_window_position(
                anchor_hwnd,
                window.get_panel_width(),
                window.get_panel_height(),
            ) {
                window.window().set_position(pos);
            }
            window.show()?;
            win_focus::keep_toast_inactive(window.window());
            Ok::<_, slint::PlatformError>(window)
        })();

        #[cfg(windows)]
        CREATING_DEBUG_WINDOW.with(|flag| flag.set(false));

        let window = match created {
            Ok(w) => w,
            Err(err) => {
                eprintln!("Could not open Spotlight debug log: {err}");
                return;
            }
        };

        {
            let ctrl = Rc::downgrade(self);
            window.on_close_requested(move || {
                if let Some(ctrl) = ctrl.upgrade() {
                    ctrl.close();
                }
            });
        }
        {
            let ctrl = Rc::downgrade(self);
            window.window().on_close_requested(move || {
                if let Some(ctrl) = ctrl.upgrade() {
                    ctrl.close();
                }
                slint::CloseRequestResponse::HideWindow
            });
        }
        {
            let ctrl = Rc::downgrade(self);
            window.on_clear_requested(move || {
                clear_logs();
                if let Some(ctrl) = ctrl.upgrade() {
                    ctrl.sync_ui(true);
                }
            });
        }
        {
            let ctrl = Rc::downgrade(self);
            window.on_toggle_pause(move || {
                if let Some(ctrl) = ctrl.upgrade() {
                    let next = !ctrl.paused.get();
                    ctrl.paused.set(next);
                    if let Some(win) = ctrl.window.borrow().as_ref() {
                        win.set_paused(next);
                    }
                    if !next {
                        ctrl.sync_ui(true);
                    }
                }
            });
        }

        window.set_paused(self.paused.get());
        *self.window.borrow_mut() = Some(window);
        *self.last_sample.borrow_mut() = Some(SampleSnapshot::capture());
        self.sync_ui(true);

        let ctrl = Rc::downgrade(self);
        self.refresh_timer
            .start(TimerMode::Repeated, Duration::from_secs(1), move || {
                if let Some(ctrl) = ctrl.upgrade() {
                    ctrl.sync_ui(false);
                }
            });
    }

    fn sync_ui(&self, force_logs: bool) {
        let win_ref = self.window.borrow();
        let Some(window) = win_ref.as_ref() else {
            return;
        };

        let now = SampleSnapshot::capture();
        let mut prev_slot = self.last_sample.borrow_mut();
        if let Some(prev) = prev_slot.as_ref() {
            let secs = now.at.saturating_duration_since(prev.at).as_secs_f64();
            if secs >= 0.2 {
                let ui_hz = ((now.ui_ticks - prev.ui_ticks) as f64 / secs).round() as u64;
                let ws_hz = ((now.ws_polls - prev.ws_polls) as f64 / secs).round() as u64;
                let fg_hz = ((now.fg_hooks - prev.fg_hooks) as f64 / secs).round() as u64;
                window.set_ui_timer_rate(SharedString::from(format!("{ui_hz}/s")));
                window.set_ws_poll_rate(SharedString::from(format!("{ws_hz}/s")));
                window.set_fg_hook_rate(SharedString::from(format!("{fg_hz}/s")));
                *prev_slot = Some(now);
            }
        } else {
            *prev_slot = Some(now);
        }

        if self.paused.get() && !force_logs {
            return;
        }

        let need_logs = force_logs || HAS_NEW_LOGS.swap(false, Ordering::AcqRel);
        if let Ok(guard) = shared().lock() {
            window.set_last_search_text(SharedString::from(guard.last_search.as_str()));
            window.set_last_rtt_text(SharedString::from(guard.last_bridge_rtt.as_str()));
            if need_logs {
                let items: Vec<DebugLogEntryUI> = guard
                    .entries
                    .iter()
                    .map(|entry| DebugLogEntryUI {
                        text: SharedString::from(entry.text.as_str()),
                        color: kind_color(entry.kind),
                    })
                    .collect();
                window.set_log_entries(ModelRc::new(VecModel::from(items)));
            }
        }
    }
}
