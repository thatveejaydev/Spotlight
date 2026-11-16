use crate::actions::{
    markers::{MarkerCommand, MarkerEvent, MarkerReply, MarkerRequest},
    Action, ActionResult,
};
use crate::catalog::{EffectEntry, ProjectItemEntry};
use crate::debug_log::{self, LogKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    env, fs,
    io::ErrorKind,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex, OnceLock,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tungstenite::{accept, client, Message};

#[cfg(windows)]
mod ws_wait {
    use std::net::TcpStream;
    use std::os::windows::io::AsRawSocket;
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Networking::WinSock::{
        WSACloseEvent, WSACreateEvent, WSAEventSelect, WSAResetEvent, WSASetEvent,
        WSAWaitForMultipleEvents, FD_CLOSE, FD_READ, SOCKET, WSAEVENT, WSA_INFINITE,
    };

    pub struct WakeSignal {
        event: WSAEVENT,
    }

    impl WakeSignal {
        pub fn new() -> Option<Arc<Self>> {
            let event = unsafe { WSACreateEvent() };
            if event == 0 {
                None
            } else {
                Some(Arc::new(Self { event }))
            }
        }

        pub fn noop() -> Arc<Self> {
            Arc::new(Self { event: 0 })
        }

        pub fn notify(&self) {
            if self.event != 0 {
                unsafe {
                    WSASetEvent(self.event as HANDLE);
                }
            }
        }

        pub fn reset(&self) {
            if self.event != 0 {
                unsafe {
                    WSAResetEvent(self.event as HANDLE);
                }
            }
        }
    }

    impl Drop for WakeSignal {
        fn drop(&mut self) {
            if self.event != 0 {
                unsafe {
                    WSACloseEvent(self.event);
                }
            }
        }
    }

    pub struct SocketWaiter {
        sock_event: WSAEVENT,
        wake: Arc<WakeSignal>,
    }

    impl SocketWaiter {
        pub fn attach(stream: &TcpStream, wake: Arc<WakeSignal>) -> Option<Self> {
            if wake.event == 0 {
                return None;
            }
            let sock_event = unsafe { WSACreateEvent() };
            if sock_event == 0 {
                return None;
            }
            let rc = unsafe {
                WSAEventSelect(
                    stream.as_raw_socket() as SOCKET,
                    sock_event,
                    (FD_READ | FD_CLOSE) as i32,
                )
            };
            if rc != 0 {
                unsafe {
                    WSACloseEvent(sock_event);
                }
                return None;
            }
            Some(Self { sock_event, wake })
        }

        pub fn reset_wake(&self) {
            self.wake.reset();
        }

        pub fn reset_socket(&self) {
            unsafe {
                WSAResetEvent(self.sock_event as HANDLE);
            }
        }

        pub fn wait(&self) {
            let events = [self.sock_event as HANDLE, self.wake.event as HANDLE];
            unsafe {
                WSAWaitForMultipleEvents(
                    events.len() as u32,
                    events.as_ptr(),
                    0,
                    WSA_INFINITE,
                    0,
                );
            }
        }
    }

    impl Drop for SocketWaiter {
        fn drop(&mut self) {
            unsafe {
                WSACloseEvent(self.sock_event);
            }
        }
    }
}

#[cfg(not(windows))]
mod ws_wait {
    use std::net::TcpStream;
    use std::sync::Arc;

    pub struct WakeSignal;

    impl WakeSignal {
        pub fn new() -> Option<Arc<Self>> {
            Some(Arc::new(Self))
        }

        pub fn noop() -> Arc<Self> {
            Arc::new(Self)
        }

        pub fn notify(&self) {}
    }

    pub struct SocketWaiter;

    impl SocketWaiter {
        pub fn attach(_stream: &TcpStream, _wake: Arc<WakeSignal>) -> Option<Self> {
            None
        }

        pub fn reset_wake(&self) {}
        pub fn reset_socket(&self) {}
        pub fn wait(&self) {}
    }
}

#[derive(Clone)]
struct PeerSender {
    tx: Sender<String>,
    wake: Arc<ws_wait::WakeSignal>,
}

impl PeerSender {
    fn new(tx: Sender<String>, wake: Arc<ws_wait::WakeSignal>) -> Self {
        Self { tx, wake }
    }

    #[cfg(test)]
    fn from_sender(tx: Sender<String>) -> Self {
        Self {
            tx,
            wake: ws_wait::WakeSignal::noop(),
        }
    }

    fn send(&self, msg: String) -> Result<(), mpsc::SendError<String>> {
        self.tx.send(msg)?;
        self.wake.notify();
        Ok(())
    }
}

pub const BRIDGE_ADDRS: [&str; 2] = ["127.0.0.1:3847", "[::1]:3847"];

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IncomingPluginMessage {
    #[serde(rename = "catalog")]
    Catalog { effects: Vec<EffectEntry> },
    #[serde(rename = "ack")]
    Ack {
        ok: bool,
        #[serde(rename = "requestId", default)]
        request_id: Option<u64>,
        #[serde(default)]
        message: String,
    },
    #[serde(rename = "project_items", rename_all = "camelCase")]
    ProjectItems {
        request_id: u64,
        project_id: Option<String>,
        sequence_id: Option<String>,
        items: Vec<ProjectItemEntry>,
    },
    #[serde(rename = "action_ack", rename_all = "camelCase")]
    ActionAck {
        request_id: u64,
        ok: bool,
        message: String,
    },
    #[serde(rename = "marker_ack", rename_all = "camelCase")]
    MarkerAck { request_id: u64, result: Option<MarkerReply>, message: String },
    #[serde(rename = "placement_ack", rename_all = "camelCase")]
    PlacementAck {
        request_id: u64,
        ok: bool,
        message: String,
    },
    #[serde(rename = "source_monitor_ack", rename_all = "camelCase")]
    SourceMonitorAck {
        request_id: u64,
        ok: bool,
        #[serde(default)]
        message: String,
    },
    #[serde(rename = "audio_preview_ack", rename_all = "camelCase")]
    AudioPreviewAck {
        request_id: u64,
        source: Option<AudioPreviewSource>,
        #[serde(default)]
        message: String,
    },
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "show_window")]
    ShowWindow,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionDurationPref {
    pub value: f64,
    pub unit: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutgoingHelperMessage {
    #[serde(rename = "request_project_items", rename_all = "camelCase")]
    RequestProjectItems { request_id: u64 },
    #[serde(rename = "place_project_item", rename_all = "camelCase")]
    PlaceProjectItem {
        request_id: u64,
        project_id: String,
        sequence_id: String,
        item_id: String,
    },
    #[serde(rename = "open_in_source_monitor", rename_all = "camelCase")]
    OpenInSourceMonitor {
        request_id: u64,
        project_id: String,
        item_id: String,
    },
    #[serde(rename = "request_audio_preview", rename_all = "camelCase")]
    RequestAudioPreview {
        request_id: u64,
        project_id: String,
        item_id: String,
    },
    #[serde(rename = "marker_command", rename_all = "camelCase")]
    MarkerCommand {
        request_id: u64,
        #[serde(flatten)]
        command: MarkerCommand,
    },
    #[serde(rename = "run_action", rename_all = "camelCase")]
    RunAction { request_id: u64, action: Action },
    #[serde(rename = "apply_effect")]
    ApplyEffect {
        #[serde(rename = "requestId")]
        request_id: u64,
        name: String,
        #[serde(rename = "matchName")]
        match_name: String,
        #[serde(rename = "effectType")]
        effect_type: String,
        #[serde(rename = "videoTransitionDuration")]
        video_transition_duration: TransitionDurationPref,
        #[serde(rename = "audioTransitionDuration")]
        audio_transition_duration: TransitionDurationPref,
    },
}

fn extract_xml_tag_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim().to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutSpec {
    pub vk: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectControlsShortcut {
    Bound(ShortcutSpec),
    Unbound,
}

impl Default for EffectControlsShortcut {
    fn default() -> Self {
        Self::Bound(default_effect_controls_shortcut())
    }
}

pub fn default_effect_controls_shortcut() -> ShortcutSpec {
    ShortcutSpec {
        vk: b'5' as u16,
        ctrl: false,
        alt: false,
        shift: true,
    }
}

pub(crate) fn find_latest_premiere_prefs_file() -> Option<PathBuf> {
    let user_profile = env::var("USERPROFILE").ok()?;
    let candidates = [
        Path::new(&user_profile).join("Documents\\Adobe\\Premiere Pro"),
        Path::new(&user_profile).join("OneDrive\\Documents\\Adobe\\Premiere Pro"),
    ];

    let mut latest_file: Option<(PathBuf, SystemTime)> = None;

    for base in &candidates {
        let Ok(ver_dirs) = fs::read_dir(base) else {
            continue;
        };
        for ver_entry in ver_dirs.flatten() {
            let ver_path = ver_entry.path();
            if !ver_path.is_dir() {
                continue;
            }
            let Ok(prof_dirs) = fs::read_dir(&ver_path) else {
                continue;
            };
            for prof_entry in prof_dirs.flatten() {
                let prof_path = prof_entry.path();
                if !prof_path.is_dir() {
                    continue;
                }
                let prefs_file = prof_path.join("Adobe Premiere Pro Prefs");
                if let Ok(meta) = fs::metadata(&prefs_file) {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if latest_file.as_ref().map_or(true, |(_, t)| mtime > *t) {
                        latest_file = Some((prefs_file, mtime));
                    }
                }
            }
        }
    }

    latest_file.map(|(p, _)| p)
}

struct CachedTransitionPrefs {
    path: PathBuf,
    mtime: SystemTime,
    scanned_at: Instant,
    prefs: (TransitionDurationPref, TransitionDurationPref),
}

fn default_transition_prefs() -> (TransitionDurationPref, TransitionDurationPref) {
    (
        TransitionDurationPref {
            value: 30.0,
            unit: "frames".to_string(),
        },
        TransitionDurationPref {
            value: 1.0,
            unit: "seconds".to_string(),
        },
    )
}

fn parse_transition_prefs_xml(xml: &str) -> (TransitionDurationPref, TransitionDurationPref) {
    let (mut video_pref, mut audio_pref) = default_transition_prefs();

    // Video Transition Default Duration:
    // BE.Prefs.DefaultVideoTransitionDuration is stored in frames when unit == 0 (or seconds if DefaultVideoTransitionDurationInSeconds is present)
    let v_unit_code = extract_xml_tag_value(xml, "BE.Prefs.DefaultVideoTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if v_unit_code == 1 {
        if let Some(sec) = extract_xml_tag_value(xml, "BE.Prefs.DefaultVideoTransitionDurationInSeconds")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            video_pref = TransitionDurationPref {
                value: sec,
                unit: "seconds".to_string(),
            };
        } else if let Some(val) = extract_xml_tag_value(xml, "BE.Prefs.DefaultVideoTransitionDuration")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            // Premiere stores DefaultVideoTransitionDuration as frames even when the UI displays Seconds
            video_pref = TransitionDurationPref {
                value: val,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(frames) = extract_xml_tag_value(xml, "BE.Prefs.DefaultVideoTransitionDuration")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
    {
        video_pref = TransitionDurationPref {
            value: frames,
            unit: "frames".to_string(),
        };
    }

    // Audio Transition Default Duration:
    // Unit == 0 -> BE.Prefs.DefaultAudioTransitionDurationInFrames
    // Unit == 1 -> BE.Prefs.DefaultAudioTransitionDurationInSeconds
    let a_unit_code = extract_xml_tag_value(xml, "BE.Prefs.DefaultAudioTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if a_unit_code == 0 {
        if let Some(frames) = extract_xml_tag_value(xml, "BE.Prefs.DefaultAudioTransitionDurationInFrames")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            audio_pref = TransitionDurationPref {
                value: frames,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(sec) = extract_xml_tag_value(xml, "BE.Prefs.DefaultAudioTransitionDurationInSeconds")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
    {
        audio_pref = TransitionDurationPref {
            value: sec,
            unit: "seconds".to_string(),
        };
    }

    (video_pref, audio_pref)
}

pub fn read_premiere_transition_prefs() -> (TransitionDurationPref, TransitionDurationPref) {
    const DIR_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
    static CACHE: OnceLock<Mutex<Option<CachedTransitionPrefs>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.scanned_at.elapsed() < DIR_RESCAN_INTERVAL {
                if let Ok(meta) = fs::metadata(&cached.path) {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime == cached.mtime {
                        return cached.prefs.clone();
                    }
                }
            }
        }
    }

    let Some(prefs_path) = find_latest_premiere_prefs_file() else {
        return default_transition_prefs();
    };
    let mtime = fs::metadata(&prefs_path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if let Ok(mut guard) = cache.lock() {
        if let Some(cached) = guard.as_mut() {
            if cached.path == prefs_path && cached.mtime == mtime {
                cached.scanned_at = Instant::now();
                return cached.prefs.clone();
            }
        }
    }

    let Ok(xml) = fs::read_to_string(&prefs_path) else {
        return default_transition_prefs();
    };
    let prefs = parse_transition_prefs_xml(&xml);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedTransitionPrefs {
            path: prefs_path,
            mtime,
            scanned_at: Instant::now(),
            prefs: prefs.clone(),
        });
    }
    prefs
}

fn decode_premiere_virtual_key(raw: u32) -> Option<u16> {
    if raw >= 0x8000_0000 {
        let vk = (raw & 0xFFFF) as u16;
        return (vk != 0).then_some(vk);
    }
    // Common Premiere internal DVA keycodes observed in .kys files.
    match raw {
        2 => Some(0x08),  // VK_BACK
        36 => Some(0x23), // VK_END
        37 => Some(0x24), // VK_HOME
        38 => Some(0x21), // VK_PRIOR (Page Up)
        39 => Some(0x22), // VK_NEXT (Page Down)
        42 => Some(0x25), // VK_LEFT
        43 => Some(0x27), // VK_RIGHT
        44 => Some(0x26), // VK_UP
        45 => Some(0x28), // VK_DOWN
        _ => None,
    }
}

fn parse_effect_controls_shortcut_kys(xml: &str) -> Option<EffectControlsShortcut> {
    parse_shortcut_kys(xml, "context.global", "uif.window.Effect Controls")
}

fn parse_shortcut_kys(xml: &str, context: &str, command: &str) -> Option<EffectControlsShortcut> {
    let open = format!("<{context}");
    let close = format!("</{context}>");
    let scope = match xml.find(&open) {
        Some(start) => match xml[start..].find(&close) {
            Some(len) => &xml[start..start + len],
            None => &xml[start..],
        },
        None if context == "context.global" => xml,
        None => return None,
    };

    let mut saw_command = false;
    let mut bindings = Vec::new();
    let mut cursor = 0;

    while let Some(rel_start) = scope[cursor..].find("<item.") {
        let item_start = cursor + rel_start;
        let Some(tag_end) = scope[item_start..].find('>') else {
            break;
        };
        let suffix_start = item_start + "<item.".len();
        let id_part = scope[suffix_start..item_start + tag_end]
            .split_whitespace()
            .next()
            .unwrap_or("");
        if id_part.is_empty() {
            cursor = item_start + tag_end + 1;
            continue;
        }
        let close_tag = format!("</item.{}>", id_part);
        let Some(rel_close) = scope[item_start + tag_end + 1..].find(&close_tag) else {
            break;
        };
        let body_start = item_start + tag_end + 1;
        let body_end = body_start + rel_close;
        let item_body = &scope[body_start..body_end];
        cursor = body_end + close_tag.len();

        if extract_xml_tag_value(item_body, "commandname").as_deref() != Some(command) {
            continue;
        }
        saw_command = true;
        let Some(raw_vk) = extract_xml_tag_value(item_body, "virtualkey")
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(vk) = decode_premiere_virtual_key(raw_vk) else {
            continue;
        };
        let ctrl = extract_xml_tag_value(item_body, "modifier.ctrl").as_deref() == Some("true");
        let alt = extract_xml_tag_value(item_body, "modifier.alt").as_deref() == Some("true");
        let shift = extract_xml_tag_value(item_body, "modifier.shift").as_deref() == Some("true");
        bindings.push(ShortcutSpec {
            vk,
            ctrl,
            alt,
            shift,
        });
    }

    if let Some(preferred) = bindings
        .iter()
        .copied()
        .find(|b| b.shift && !b.ctrl && !b.alt)
        .or_else(|| bindings.first().copied())
    {
        return Some(EffectControlsShortcut::Bound(preferred));
    }

    saw_command.then_some(EffectControlsShortcut::Unbound)
}

fn parse_preset_search_shortcuts(xml: &str) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    use EffectControlsShortcut::Bound;

    let window = parse_shortcut_kys(xml, "context.global", "uif.window.Effects");
    let Some(Bound(effects)) = window
        .filter(|binding| matches!(binding, Bound(_)))
        .or_else(|| parse_shortcut_kys(xml, "context.global", "cmd.window.effects"))
    else {
        return Err("Assign a shortcut to Effects in Premiere Keyboard Shortcuts.");
    };
    let find_box = parse_shortcut_kys(xml, "context.effects", "cmd.select.find.box")
        .or_else(|| parse_shortcut_kys(xml, "context.global", "cmd.select.find.box"));
    let Some(Bound(find_box)) = find_box else {
        return Err("Assign a shortcut to Select Find Box in Premiere Keyboard Shortcuts.");
    };
    Ok((effects, find_box))
}

pub(crate) fn read_preset_search_shortcuts(
    premiere_exe: &Path,
) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    parse_preset_search_shortcuts(&read_premiere_shortcut_profile(premiere_exe)?)
}

pub(crate) fn read_marker_shortcuts(
    premiere_exe: &Path,
) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    parse_marker_shortcuts(&read_premiere_shortcut_profile(premiere_exe)?)
}

fn parse_marker_shortcuts(xml: &str) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    use EffectControlsShortcut::Bound;
    let Some(Bound(timeline)) = parse_shortcut_kys(xml, "context.global", "uif.window.Timelines") else {
        return Err("Assign a shortcut to Timeline in Premiere Keyboard Shortcuts.");
    };
    let add_marker = parse_shortcut_kys(xml, "context.timeline", "cmd.set.marker")
        .or_else(|| parse_shortcut_kys(xml, "context.global", "cmd.set.marker"));
    let Some(Bound(add_marker)) = add_marker else {
        return Err("Assign a shortcut to Add Marker in Premiere Keyboard Shortcuts.");
    };
    Ok((timeline, add_marker))
}

fn read_premiere_shortcut_profile(premiere_exe: &Path) -> Result<String, &'static str> {
    const UNREADABLE: &str = "Could not read Premiere's keyboard shortcuts.";
    let mut name = None;
    let mut custom_path = None;
    if let Some(prefs) = find_latest_premiere_prefs_file() {
        let xml = fs::read_to_string(&prefs).map_err(|_| UNREADABLE)?;
        let document = roxmltree::Document::parse(&xml).map_err(|_| UNREADABLE)?;
        name = document
            .descendants()
            .find(|node| node.has_tag_name("FE.Prefs.Shortcuts.Filename"))
            .and_then(|node| node.text())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if let (Some(name), Some(profile)) = (&name, prefs.parent()) {
            if Path::new(name).file_name().is_none_or(|file| file != name.as_str()) {
                return Err(UNREADABLE);
            }
            custom_path = Some(profile.join("Win").join(name));
        }
    }

    let xml = match custom_path.map(fs::read_to_string) {
        Some(Ok(xml)) => xml,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => return Err(UNREADABLE),
        _ => {
            // Premiere stores its bundled shortcut sets beside the executable.
            // The command identifiers in those sets are language independent.
            let bundled = premiere_exe
                .parent()
                .ok_or(UNREADABLE)?
                .join("Keyboard Shortcuts")
                .join("en");
            let name = name.as_deref().unwrap_or("Adobe Premiere Pro Defaults.kys");
            fs::read_to_string(bundled.join(name)).map_err(|_| UNREADABLE)?
        }
    };
    Ok(xml)
}

struct CachedEffectControlsShortcut {
    prefs_path: PathBuf,
    prefs_mtime: SystemTime,
    kys_path: Option<PathBuf>,
    kys_mtime: SystemTime,
    scanned_at: Instant,
    shortcut: EffectControlsShortcut,
}

pub fn read_effect_controls_shortcut() -> EffectControlsShortcut {
    const DIR_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
    static CACHE: OnceLock<Mutex<Option<CachedEffectControlsShortcut>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    let file_mtime = |path: &Path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };

    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.scanned_at.elapsed() < DIR_RESCAN_INTERVAL
                && cached.prefs_path.exists()
                && file_mtime(&cached.prefs_path) == cached.prefs_mtime
            {
                let kys_unchanged = match &cached.kys_path {
                    Some(kys) => kys.exists() && file_mtime(kys) == cached.kys_mtime,
                    None => true,
                };
                if kys_unchanged {
                    return cached.shortcut;
                }
            }
        }
    }

    let Some(prefs_path) = find_latest_premiere_prefs_file() else {
        return EffectControlsShortcut::default();
    };
    let prefs_mtime = file_mtime(&prefs_path);
    let Ok(prefs_xml) = fs::read_to_string(&prefs_path) else {
        return EffectControlsShortcut::default();
    };

    let kys_path = extract_xml_tag_value(&prefs_xml, "FE.Prefs.Shortcuts.Filename")
        .filter(|name| !name.is_empty())
        .and_then(|name| prefs_path.parent().map(|dir| dir.join("Win").join(name)))
        .filter(|path| path.is_file());

    let kys_mtime = kys_path
        .as_deref()
        .map(file_mtime)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if let Ok(mut guard) = cache.lock() {
        if let Some(cached) = guard.as_mut() {
            if cached.prefs_path == prefs_path
                && cached.prefs_mtime == prefs_mtime
                && cached.kys_path == kys_path
                && cached.kys_mtime == kys_mtime
            {
                cached.scanned_at = Instant::now();
                return cached.shortcut;
            }
        }
    }

    let shortcut = kys_path
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|xml| parse_effect_controls_shortcut_kys(&xml))
        .unwrap_or_default();

    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedEffectControlsShortcut {
            prefs_path,
            prefs_mtime,
            kys_path,
            kys_mtime,
            scanned_at: Instant::now(),
            shortcut,
        });
    }
    shortcut
}

pub enum BridgeEvent {
    ClientConnected,
    ClientDisconnected,
    CatalogReceived(Vec<EffectEntry>),
    ApplyResult(ApplyResult),
    ActionResult(ActionResult),
    MarkerResult(MarkerEvent),
    ProjectItemsChanged,
    PlacementResult(PlacementResult),
    SourceMonitorResult(PlacementResult),
    AudioPreviewResult(AudioPreviewResult),
    ShowWindowRequested,
}

pub struct ApplyResult {
    pub peer_id: usize,
    pub request_id: Option<u64>,
    pub ok: bool,
    pub message: String,
}

pub struct PlacementResult {
    pub peer_id: usize,
    pub request_id: u64,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPreviewSource {
    pub path: std::path::PathBuf,
    pub in_seconds: Option<f64>,
    pub out_seconds: Option<f64>,
}

pub struct AudioPreviewResult {
    pub peer_id: usize,
    pub request_id: u64,
    pub source: Option<AudioPreviewSource>,
    pub message: String,
}

/// Checks if another instance of Premiere Spotlight is already listening on 127.0.0.1:3847.
/// If so, optionally sends `{"type":"show_window"}` to wake its UI and returns `true`.
pub fn check_or_wake_existing_instance(show_window: bool) -> bool {
    let Ok(addr) = "127.0.0.1:3847".parse::<SocketAddr>() else {
        return false;
    };
    let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(80)) else {
        return false;
    };

    if show_window {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
        if let Ok((mut ws, _)) = client("ws://127.0.0.1:3847", stream) {
            let _ = ws.send(Message::Text(r#"{"type":"show_window"}"#.to_string()));
            let _ = ws.flush();
            let _ = ws.close(None);
        }
    }

    true
}

#[derive(Clone, Default)]
pub struct BridgeClients {
    peers: Arc<Mutex<Vec<Peer>>>,
    next_id: Arc<AtomicUsize>,
    in_flight: Arc<Mutex<HashMap<u64, (&'static str, Instant)>>>,
}

struct Peer {
    id: usize,
    tx: PeerSender,
    media: Option<MediaCatalog>,
    requested: u64,
}

#[derive(Clone)]
pub struct MediaCatalog {
    pub peer_id: usize,
    pub project_id: Option<String>,
    pub sequence_id: Option<String>,
    pub items: Vec<ProjectItemEntry>,
}

fn sole_marker_peer(peers: &[Peer], expected: Option<usize>) -> Result<&Peer, &'static str> {
    match peers {
        [peer] if expected.is_none_or(|id| id == peer.id) => Ok(peer),
        [] | [_] => Err("The Premiere marker bridge disconnected."),
        _ => Err("Leave one Premiere bridge connected to use sequence markers."),
    }
}

fn project_source<'a>(
    peers: &'a [Peer],
    catalog: &MediaCatalog,
    item_id: &str,
) -> Result<(&'a Peer, &'a ProjectItemEntry, &'a str), &'static str> {
    let mut catalogs = peers.iter().filter_map(|peer| Some((peer, peer.media.as_ref()?)));
    let (peer, current) = catalogs.next().ok_or("Premiere media bridge disconnected.")?;
    if catalogs.next().is_some() {
        return Err("Close the other Premiere session before opening media.");
    }
    if peer.id != catalog.peer_id || current.project_id != catalog.project_id {
        return Err("Project media changed. Search again.");
    }
    let entry = current.items.iter().find(|entry| entry.item_id == item_id)
        .ok_or("Project media changed. Search again.")?;
    let project_id = current.project_id.as_deref().ok_or("Open a Premiere project first.")?;
    Ok((peer, entry, project_id))
}

impl BridgeClients {
    pub fn new() -> Self {
        Self::default()
    }

    fn track_request(&self, request_id: u64, label: &'static str) {
        if let Ok(mut map) = self.in_flight.lock() {
            if map.len() > 256 {
                let cutoff = Instant::now() - Duration::from_secs(30);
                map.retain(|_, (_, started)| *started > cutoff);
            }
            map.insert(request_id, (label, Instant::now()));
        }
    }

    fn finish_request(&self, request_id: u64) {
        let entry = self
            .in_flight
            .lock()
            .ok()
            .and_then(|mut map| map.remove(&request_id));
        if let Some((label, started)) = entry {
            debug_log::record_bridge_rtt(label, started.elapsed());
        }
    }

    #[cfg(test)]
    pub(crate) fn connect_test_peer(&self, catalog: MediaCatalog) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        self.peers.lock().unwrap().push(Peer {
            id: catalog.peer_id,
            tx: PeerSender::from_sender(tx),
            media: Some(catalog),
            requested: 0,
        });
        rx
    }

    pub fn media_catalog(&self) -> Result<Option<MediaCatalog>, &'static str> {
        let peers = self.peers.lock().unwrap();
        let mut catalogs = peers.iter().filter_map(|peer| peer.media.as_ref());
        let first = catalogs.next();
        if catalogs.next().is_some() {
            return Err("Multiple Premiere sessions. Close the other session to place media.");
        }
        Ok(first.cloned())
    }

    pub fn request_project_items(&self) {
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::RequestProjectItems { request_id };
        let json = serde_json::to_string(&msg).unwrap();
        let mut sent = 0usize;
        for peer in self.peers.lock().unwrap().iter_mut() {
            peer.requested = request_id;
            if let Some(media) = &mut peer.media {
                media.items.clear();
            }
            if peer.tx.send(json.clone()).is_ok() {
                sent += 1;
            }
        }
        if sent > 0 {
            self.track_request(request_id, "project_items");
            debug_log::log_event(
                LogKind::Bridge,
                "BRIDGE",
                format!("-> request_project_items (id={request_id})"),
            );
        }
    }

    pub fn place_project_item(
        &self,
        catalog: &MediaCatalog,
        item_id: String,
    ) -> Result<u64, &'static str> {
        let peers = self.peers.lock().unwrap();
        let mut media_peers = peers.iter().filter(|peer| peer.media.is_some());
        let Some(peer) = media_peers.next() else {
            return Err("Premiere media bridge disconnected.");
        };
        if media_peers.next().is_some() {
            return Err("Close the other Premiere session before placing media.");
        }
        let current = peer.media.as_ref().unwrap();
        if peer.id != catalog.peer_id
            || current.project_id != catalog.project_id
            || current.sequence_id != catalog.sequence_id
        {
            return Err("Project media changed. Search again.");
        }
        let entry = current.items.iter().find(|entry| entry.item_id == item_id)
            .ok_or("Project media changed. Search again.")?;
        if let Some(message) = entry.placement_error() {
            return Err(message);
        }
        let project_id = current
            .project_id
            .clone()
            .ok_or("Open a Premiere project first.")?;
        let sequence_id = current
            .sequence_id
            .clone()
            .ok_or("Open a timeline before placing media.")?;
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::PlaceProjectItem {
            request_id,
            project_id,
            sequence_id,
            item_id,
        };
        peer.tx
            .send(serde_json::to_string(&msg).unwrap())
            .map_err(|_| "Premiere media bridge disconnected.")?;
        self.track_request(request_id, "place_project_item");
        debug_log::log_event(
            LogKind::Bridge,
            "BRIDGE",
            format!("-> place_project_item (id={request_id})"),
        );
        Ok(request_id)
    }

    pub fn open_in_source_monitor(
        &self,
        catalog: &MediaCatalog,
        item_id: String,
    ) -> Result<u64, &'static str> {
        let peers = self.peers.lock().unwrap();
        let (peer, entry, project_id) = project_source(&peers, catalog, &item_id)?;
        if let Some(message) = entry.source_monitor_error() {
            return Err(message);
        }
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::OpenInSourceMonitor {
            request_id,
            project_id: project_id.to_owned(),
            item_id,
        };
        peer.tx
            .send(serde_json::to_string(&msg).unwrap())
            .map_err(|_| "Premiere media bridge disconnected.")?;
        self.track_request(request_id, "open_in_source_monitor");
        debug_log::log_event(
            LogKind::Bridge,
            "BRIDGE",
            format!("-> open_in_source_monitor (id={request_id})"),
        );
        Ok(request_id)
    }

    pub fn request_audio_preview(
        &self,
        catalog: &MediaCatalog,
        item_id: &str,
    ) -> Result<u64, &'static str> {
        let peers = self.peers.lock().unwrap();
        let (peer, entry, project_id) = project_source(&peers, catalog, item_id)?;
        if entry.kind != crate::catalog::ProjectItemKind::Media {
            return Err("Select an audio clip to preview.");
        }
        if entry.offline {
            return Err("Relink the offline media before previewing it.");
        }
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::RequestAudioPreview {
            request_id,
            project_id: project_id.to_owned(),
            item_id: item_id.to_owned(),
        };
        peer.tx
            .send(serde_json::to_string(&msg).unwrap())
            .map_err(|_| "Premiere media bridge disconnected.")?;
        self.track_request(request_id, "request_audio_preview");
        Ok(request_id)
    }

    pub fn marker_peer_id(&self) -> Result<usize, &'static str> {
        let peers = self.peers.lock().unwrap();
        sole_marker_peer(&peers, None).map(|peer| peer.id)
    }

    pub fn marker_command(
        &self,
        command: MarkerCommand,
        expected_peer: Option<usize>,
    ) -> Result<MarkerRequest, &'static str> {
        let peers = self.peers.lock().unwrap();
        let peer = sole_marker_peer(&peers, expected_peer)?;
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let message = OutgoingHelperMessage::MarkerCommand { request_id, command };
        let json = serde_json::to_string(&message).map_err(|_| "Could not prepare the marker command.")?;
        peer.tx.send(json).map_err(|_| "The Premiere marker bridge disconnected.")?;
        self.track_request(request_id, "marker_command");
        Ok(MarkerRequest::new(peer.id, request_id))
    }

    pub fn run_action(&self, action: Action) -> Result<(u64, usize), &'static str> {
        let peers = self.peers.lock().unwrap();
        let peer = match peers.as_slice() {
            [peer] => peer,
            [] => return Err("Premiere action bridge disconnected."),
            _ => return Err("Multiple Premiere bridges are connected. Leave one connected to run an action."),
        };
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let message = OutgoingHelperMessage::RunAction { request_id, action };
        let json = serde_json::to_string(&message).map_err(|_| "Could not prepare the action.")?;
        peer.tx.send(json).map_err(|_| "Premiere action bridge disconnected.")?;
        self.track_request(request_id, "run_action");
        debug_log::log_event(
            LogKind::Bridge,
            "BRIDGE",
            format!("-> run_action \"{}\" (id={request_id})", action.name()),
        );
        Ok((request_id, peer.id))
    }

    pub fn send_apply(&self, entry: &EffectEntry) -> Option<(u64, Vec<usize>)> {
        if entry.is_preset() {
            return None;
        }
        let (video_transition_duration, audio_transition_duration) =
            if entry.effect_type == "videoTransition" || entry.effect_type == "audioTransition" {
                read_premiere_transition_prefs()
            } else {
                default_transition_prefs()
            };

        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::ApplyEffect {
            request_id,
            name: entry.name.clone(),
            match_name: entry.match_name.clone(),
            effect_type: entry.effect_type.clone(),
            video_transition_duration,
            audio_transition_duration,
        };
        let Ok(json) = serde_json::to_string(&msg) else {
            return None;
        };

        let peers = self.peers.lock().unwrap();
        let recipients: Vec<_> = peers
            .iter()
            .filter(|peer| peer.tx.send(json.clone()).is_ok())
            .map(|peer| peer.id)
            .collect();
        if !recipients.is_empty() {
            self.track_request(request_id, "apply_effect");
            debug_log::log_event(
                LogKind::Bridge,
                "BRIDGE",
                format!("-> apply_effect \"{}\" (id={request_id})", entry.name),
            );
            Some((request_id, recipients))
        } else {
            None
        }
    }

    pub fn start_server<F>(&self, on_event: F)
    where
        F: Fn(BridgeEvent) + Send + Sync + 'static,
    {
        let callback = Arc::new(on_event);

        for addr in BRIDGE_ADDRS {
            let bridge = self.clone();
            let cb = callback.clone();
            let bind_addr = addr.to_string();

            thread::spawn(move || {
                let Ok(listener) = TcpListener::bind(&bind_addr) else {
                    eprintln!("[bridge] Could not bind WebSocket on {}", bind_addr);
                    return;
                };
                println!(
                    "[bridge] Listening for Premiere UXP plugin on ws://{}",
                    bind_addr
                );

                for stream in listener.incoming().flatten() {
                    let bridge = bridge.clone();
                    let cb_ref = cb.clone();

                    thread::spawn(move || {
                        handle_client(stream, bridge, cb_ref);
                    });
                }
            });
        }
    }
}

fn handle_client<F>(stream: TcpStream, bridge: BridgeClients, cb: Arc<F>)
where
    F: Fn(BridgeEvent) + Send + Sync + 'static,
{
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let Ok(mut ws) = accept(stream) else {
        return;
    };

    let (tx, rx) = mpsc::channel::<String>();
    let wake = ws_wait::WakeSignal::new().unwrap_or_else(ws_wait::WakeSignal::noop);
    let peer_tx = PeerSender::new(tx, wake.clone());
    let peer_id = bridge.next_id.fetch_add(1, Ordering::Relaxed);
    let mut is_plugin_client = false;

    // Ask plugin to send its catalog right away before switching socket to non-blocking wait.
    let _ = ws.send(Message::Text(r#"{"type":"request_catalog"}"#.to_string()));
    let _ = ws.flush();

    let waiter = ws_wait::SocketWaiter::attach(ws.get_ref(), wake);
    if waiter.is_none() {
        // Non-Windows fallback when OS socket event multiplexing is unavailable.
        let _ = ws
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(25)));
    }

    'client_loop: loop {
        if let Some(waiter) = &waiter {
            waiter.reset_wake();
        }

        // 1. Drain any outbound commands queued from the UI thread
        while let Ok(outbound_json) = rx.try_recv() {
            if ws.send(Message::Text(outbound_json)).is_err() || ws.flush().is_err() {
                break 'client_loop;
            }
        }

        if let Some(waiter) = &waiter {
            waiter.reset_socket();
        }

        // 2. Drain incoming messages from the UXP plugin (or wake-up ping)
        loop {
            match ws.read() {
                Ok(Message::Text(txt)) => {
                    if let Ok(parsed) = serde_json::from_str::<IncomingPluginMessage>(&txt) {
                        if !is_plugin_client
                            && !matches!(
                                parsed,
                                IncomingPluginMessage::ShowWindow
                                    | IncomingPluginMessage::Ack { .. }
                                    | IncomingPluginMessage::ActionAck { .. }
                                    | IncomingPluginMessage::MarkerAck { .. }
                                    | IncomingPluginMessage::PlacementAck { .. }
                                    | IncomingPluginMessage::SourceMonitorAck { .. }
                                    | IncomingPluginMessage::AudioPreviewAck { .. }
                            )
                        {
                            is_plugin_client = true;
                            bridge.peers.lock().unwrap().push(Peer {
                                id: peer_id,
                                tx: peer_tx.clone(),
                                media: None,
                                requested: 0,
                            });
                            debug_log::log_event(
                                LogKind::Bridge,
                                "BRIDGE",
                                format!("Peer #{peer_id} connected"),
                            );
                            cb(BridgeEvent::ClientConnected);
                        }
                        match parsed {
                            IncomingPluginMessage::Catalog { effects } => {
                                debug_log::log_event(
                                    LogKind::Bridge,
                                    "BRIDGE",
                                    format!("<- catalog ({} effects)", effects.len()),
                                );
                                cb(BridgeEvent::CatalogReceived(effects));
                            }
                            IncomingPluginMessage::Ack { request_id, ok, message } => {
                                if let Some(id) = request_id {
                                    bridge.finish_request(id);
                                }
                                if is_plugin_client {
                                    cb(BridgeEvent::ApplyResult(ApplyResult {
                                        peer_id, request_id, ok, message,
                                    }));
                                }
                            }
                            IncomingPluginMessage::ProjectItems {
                                request_id,
                                project_id,
                                sequence_id,
                                items,
                            } => {
                                let item_count = items.len();
                                let mut peers = bridge.peers.lock().unwrap();
                                let peer = peers.iter_mut().find(|peer| peer.id == peer_id).unwrap();
                                if peer.accept_catalog(request_id, project_id, sequence_id, items) {
                                    drop(peers);
                                    bridge.finish_request(request_id);
                                    debug_log::log_event(
                                        LogKind::Bridge,
                                        "BRIDGE",
                                        format!("<- project_items (id={request_id}, {item_count} items)"),
                                    );
                                    cb(BridgeEvent::ProjectItemsChanged);
                                }
                            }
                            IncomingPluginMessage::ActionAck { request_id, ok, message } => {
                                if is_plugin_client {
                                    bridge.finish_request(request_id);
                                    cb(BridgeEvent::ActionResult(ActionResult {
                                        peer_id, request_id, ok, message,
                                    }));
                                }
                            }
                            IncomingPluginMessage::MarkerAck { request_id, result, message } => {
                                if is_plugin_client {
                                    bridge.finish_request(request_id);
                                    cb(BridgeEvent::MarkerResult(MarkerEvent { peer_id, request_id, result, message }));
                                }
                            }
                            IncomingPluginMessage::PlacementAck {
                                request_id,
                                ok,
                                message,
                            } => {
                                bridge.finish_request(request_id);
                                if is_plugin_client {
                                    cb(BridgeEvent::PlacementResult(PlacementResult {
                                        peer_id,
                                        request_id,
                                        ok,
                                        message,
                                    }));
                                }
                            }
                            IncomingPluginMessage::SourceMonitorAck {
                                request_id,
                                ok,
                                message,
                            } => {
                                bridge.finish_request(request_id);
                                if is_plugin_client {
                                    cb(BridgeEvent::SourceMonitorResult(PlacementResult {
                                        peer_id,
                                        request_id,
                                        ok,
                                        message,
                                    }));
                                }
                            }
                            IncomingPluginMessage::AudioPreviewAck { request_id, source, message } => {
                                bridge.finish_request(request_id);
                                if is_plugin_client {
                                    cb(BridgeEvent::AudioPreviewResult(AudioPreviewResult {
                                        peer_id, request_id, source, message,
                                    }));
                                }
                            }
                            IncomingPluginMessage::Ping => {}
                            IncomingPluginMessage::ShowWindow => {
                                cb(BridgeEvent::ShowWindowRequested);
                                break 'client_loop;
                            }
                        }
                    }
                }
                Ok(Message::Ping(payload)) => {
                    let _ = ws.send(Message::Pong(payload));
                    let _ = ws.flush();
                }
                Ok(Message::Close(_)) => break 'client_loop,
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
                {
                    if waiter.is_none() {
                        debug_log::record_bridge_timeout();
                    }
                    break;
                }
                Err(_) => break 'client_loop,
            }
        }

        if let Some(waiter) = &waiter {
            waiter.wait();
        }
    }

    if is_plugin_client {
        debug_log::log_event(
            LogKind::Bridge,
            "BRIDGE",
            format!("Peer #{peer_id} disconnected"),
        );
        let mut peers = bridge.peers.lock().unwrap();
        peers.retain(|peer| peer.id != peer_id);
        cb(BridgeEvent::ProjectItemsChanged);
        if peers.is_empty() {
            cb(BridgeEvent::ClientDisconnected);
        }
    }
}

impl Peer {
    fn accept_catalog(
        &mut self,
        request_id: u64,
        project_id: Option<String>,
        sequence_id: Option<String>,
        items: Vec<ProjectItemEntry>,
    ) -> bool {
        if request_id == self.requested {
            self.media = Some(MediaCatalog {
                peer_id: self.id,
                project_id,
                sequence_id,
                items,
            });
            true
        } else if request_id == 0 && self.media.is_none() {
            // Capability announcement can precede the response to the latest refresh.
            self.media = Some(MediaCatalog {
                peer_id: self.id,
                project_id: None,
                sequence_id: None,
                items: Vec::new(),
            });
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_commands_and_acknowledgments_support_request_ids_and_legacy_replies() {
        let bridge = BridgeClients::new();
        let (tx, rx) = mpsc::channel();
        bridge.peers.lock().unwrap().push(Peer {
            id: 7,
            tx: PeerSender::from_sender(tx),
            media: None,
            requested: 0,
        });
        let effect = EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "blur".into(),
            effect_type: "videoEffect".into(),
            folder: String::new(),
        };
        let (id, recipients) = bridge.send_apply(&effect).unwrap();
        assert_eq!(recipients, [7]);
        let command: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(command["requestId"], id);
        for request_id in [Some(id), None] {
            let mut reply = serde_json::json!({"type":"ack","ok":true,"message":"Applied effect"});
            if let Some(id) = request_id {
                reply["requestId"] = id.into();
            }
            assert!(matches!(
                serde_json::from_value::<IncomingPluginMessage>(reply).unwrap(),
                IncomingPluginMessage::Ack { request_id: actual, ok: true, .. } if actual == request_id
            ));
        }
    }

    #[test]
    fn media_placement_is_scoped_to_the_current_catalog_and_peer() {
        let bridge = BridgeClients::new();
        let (tx, rx) = mpsc::channel();
        let IncomingPluginMessage::ProjectItems { request_id, project_id, sequence_id, items } =
            serde_json::from_str(r#"{"type":"project_items","requestId":3,"projectId":"p","sequenceId":"s","items":[
                {"itemId":"i","name":"Clip","binPath":"Bin","kind":"media"},
                {"itemId":"b","name":"Bin","binPath":"Project root","kind":"bin"},
                {"itemId":"a","name":"Active","binPath":"Project root","kind":"activeSequence"},
                {"itemId":"o","name":"Other","binPath":"Project root","kind":"other"},
                {"itemId":"f","name":"Offline","binPath":"Project root","kind":"media","offline":true}
            ]}"#).unwrap()
        else {
            panic!("expected a media catalog");
        };
        let mut peer = Peer {
            id: 42,
            tx: PeerSender::from_sender(tx),
            media: None,
            requested: request_id,
        };
        assert!(peer.accept_catalog(request_id, project_id, sequence_id, items));
        assert!(!peer.accept_catalog(request_id - 1, None, None, vec![]));
        bridge.peers.lock().unwrap().push(peer);
        let catalog = bridge.media_catalog().unwrap().unwrap();
        for (item_id, message) in [
            ("b", "Bins can't be placed on the timeline. Bins are search only."),
            ("a", "A sequence can't nest into itself."),
            ("o", "This project item can't be placed on the timeline."),
            ("f", "Relink the offline media before placing it."),
        ] {
            assert_eq!(bridge.place_project_item(&catalog, item_id.into()), Err(message));
        }
        assert!(rx.try_recv().is_err());
        let id = bridge.place_project_item(&catalog, "i".into()).unwrap();
        let message: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            message,
            serde_json::json!({
                "type": "place_project_item",
                "requestId": id,
                "projectId": "p",
                "sequenceId": "s",
                "itemId": "i",
            })
        );

        let (tx, other_rx) = mpsc::channel();
        bridge.peers.lock().unwrap().push(Peer {
            id: 43,
            tx: PeerSender::from_sender(tx),
            media: Some(MediaCatalog {
                peer_id: 43,
                project_id: None,
                sequence_id: None,
                items: Vec::new(),
            }),
            requested: 0,
        });
        assert!(bridge.media_catalog().is_err());
        assert!(bridge.place_project_item(&catalog, "i".into()).is_err());
        assert!(rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
        bridge.peers.lock().unwrap().pop();

        bridge.request_project_items();
        let message: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(message["type"], "request_project_items");
        assert!(bridge.place_project_item(&catalog, "i".into()).is_err());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn source_monitor_requests_need_a_project_but_not_a_timeline() {
        use crate::catalog::ProjectItemKind;
        let item = |id: &str, kind| ProjectItemEntry {
            item_id: id.into(),
            name: id.into(),
            bin_path: "Project root".into(),
            kind,
            offline: id == "f",
        };
        let catalog = MediaCatalog {
            peer_id: 9,
            project_id: Some("p".into()),
            sequence_id: None,
            items: vec![
                item("i", ProjectItemKind::Media),
                item("s", ProjectItemKind::Sequence),
                item("a", ProjectItemKind::ActiveSequence),
                item("f", ProjectItemKind::Media),
                item("b", ProjectItemKind::Bin),
                item("o", ProjectItemKind::Other),
            ],
        };
        let bridge = BridgeClients::new();
        let rx = bridge.connect_test_peer(catalog.clone());
        for (item_id, message) in [
            ("b", "Bins can't open in the Source Monitor."),
            ("o", "This project item can't open in the Source Monitor."),
            ("missing", "Project media changed. Search again."),
        ] {
            assert_eq!(bridge.open_in_source_monitor(&catalog, item_id.into()), Err(message));
        }
        assert!(rx.try_recv().is_err());
        for item_id in ["i", "s", "a", "f"] {
            let id = bridge.open_in_source_monitor(&catalog, item_id.into()).unwrap();
            let message: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
            assert_eq!(
                message,
                serde_json::json!({
                    "type": "open_in_source_monitor",
                    "requestId": id,
                    "projectId": "p",
                    "itemId": item_id,
                })
            );
        }
        let mut other_project = catalog.clone();
        other_project.project_id = Some("q".into());
        assert!(bridge.open_in_source_monitor(&other_project, "i".into()).is_err());
        assert!(matches!(
            serde_json::from_str::<IncomingPluginMessage>(
                r#"{"type":"source_monitor_ack","requestId":4,"ok":false,"message":"nope"}"#
            ).unwrap(),
            IncomingPluginMessage::SourceMonitorAck { request_id: 4, ok: false, .. }
        ));
    }

    #[test]
    fn websocket_bridge_wakes_immediately_on_outbound_command() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let bridge = BridgeClients::new();
        let server_bridge = bridge.clone();
        let (connected_tx, connected_rx) = mpsc::channel();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let cb = Arc::new(move |event: BridgeEvent| {
                if matches!(event, BridgeEvent::ClientConnected) {
                    let _ = connected_tx.send(());
                }
            });
            handle_client(stream, server_bridge, cb);
        });

        let stream = TcpStream::connect(addr).unwrap();
        let (mut ws, _) = client(format!("ws://{addr}"), stream).unwrap();
        // Read initial request_catalog sent on accept
        assert!(matches!(ws.read().unwrap(), Message::Text(t) if t.contains("request_catalog")));

        ws.send(Message::Text(r#"{"type":"ping"}"#.into())).unwrap();
        ws.flush().unwrap();
        connected_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // Let the server sleep idle for 100ms, then send an outbound command and verify fast wakeup.
        thread::sleep(Duration::from_millis(100));
        let sent_at = Instant::now();
        bridge.request_project_items();
        ws.get_ref()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let msg = ws.read().unwrap();
        let rtt = sent_at.elapsed();
        assert!(matches!(msg, Message::Text(t) if t.contains("request_project_items")));
        assert!(
            rtt < Duration::from_millis(200),
            "expected fast event-driven wakeup, got {rtt:?}"
        );

        let _ = ws.close(None);
        server.join().unwrap();
    }

    #[test]
    fn marker_shortcuts_respect_the_timeline_override_and_unbound_commands() {
        let profile = r#"<context.global>
          <item.1><commandname>uif.window.Timelines</commandname><modifier.shift>true</modifier.shift><virtualkey>2147483699</virtualkey></item.1>
          <item.2><commandname>cmd.set.marker</commandname><virtualkey>2147483725</virtualkey></item.2>
        </context.global>"#;
        let (timeline, add) = parse_marker_shortcuts(profile).unwrap();
        assert_eq!(timeline.vk, u16::from(b'3'));
        assert!(timeline.shift);
        assert_eq!(add.vk, u16::from(b'M'));

        let custom = format!(r#"{profile}<context.timeline>
          <item.3><commandname>cmd.set.marker</commandname><modifier.alt>true</modifier.alt><virtualkey>2147483726</virtualkey></item.3>
        </context.timeline>"#);
        let (_, add) = parse_marker_shortcuts(&custom).unwrap();
        assert_eq!(add.vk, u16::from(b'N'));
        assert!(add.alt);
        assert!(parse_marker_shortcuts(&custom.replace("<virtualkey>2147483726</virtualkey>", "")).is_err());
    }

    #[test]
    fn parses_effect_controls_shortcut_from_kys() {
        // Prefers Shift-only binding when multiple bindings exist (like [Custom].kys).
        let multi = r#"
        <PremiereData Version="3"><shortcuts Version="5">
          <context.global Version="1">
            <item.881 Version="1">
              <virtualkey>2147483698</virtualkey>
              <modifier.ctrl>false</modifier.ctrl>
              <modifier.alt>false</modifier.alt>
              <modifier.shift>false</modifier.shift>
              <commandname>uif.window.Effect Controls</commandname>
            </item.881>
            <item.880 Version="1">
              <virtualkey>2147483701</virtualkey>
              <modifier.ctrl>false</modifier.ctrl>
              <modifier.alt>false</modifier.alt>
              <modifier.shift>true</modifier.shift>
              <commandname>uif.window.Effect Controls</commandname>
            </item.880>
          </context.global>
        </shortcuts></PremiereData>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(multi),
            Some(EffectControlsShortcut::Bound(ShortcutSpec {
                vk: b'5' as u16,
                ctrl: false,
                alt: false,
                shift: true,
            }))
        );

        // Uses single custom binding when Shift+5 is remapped.
        let custom = r#"
        <context.global Version="1">
          <item.10 Version="1">
            <virtualkey>2147483717</virtualkey>
            <modifier.ctrl>true</modifier.ctrl>
            <modifier.alt>false</modifier.alt>
            <modifier.shift>false</modifier.shift>
            <commandname>uif.window.Effect Controls</commandname>
          </item.10>
        </context.global>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(custom),
            Some(EffectControlsShortcut::Bound(ShortcutSpec {
                vk: b'E' as u16,
                ctrl: true,
                alt: false,
                shift: false,
            }))
        );

        // Recognizes explicitly unbound command.
        let unbound = r#"
        <context.global Version="1">
          <item.880 Version="1">
            <commandname>uif.window.Effect Controls</commandname>
          </item.880>
        </context.global>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(unbound),
            Some(EffectControlsShortcut::Unbound)
        );

        // Missing entry falls back via None -> Default.
        let missing = r#"<context.global Version="1"></context.global>"#;
        assert_eq!(parse_effect_controls_shortcut_kys(missing), None);
    }

    #[test]
    fn uses_premiere_bindings_for_effects_and_the_find_box() {
        let shortcuts = r#"
        <context.global Version="1">
          <item.10 Version="1">
            <commandname>uif.window.Effects</commandname>
            <virtualkey>2147483717</virtualkey>
            <modifier.ctrl>true</modifier.ctrl>
          </item.10>
          <item.11 Version="1">
            <commandname>cmd.select.find.box</commandname>
            <virtualkey>2147483718</virtualkey>
            <modifier.shift>true</modifier.shift>
          </item.11>
        </context.global>
        <context.effects Version="1">
          <item.0 Version="1">
            <commandname>cmd.select.find.box</commandname>
            <virtualkey>2147483731</virtualkey>
            <modifier.alt>true</modifier.alt>
          </item.0>
        </context.effects>"#;
        let (effects, find_box) = parse_preset_search_shortcuts(shortcuts).unwrap();
        assert_eq!(
            effects,
            ShortcutSpec {
                vk: b'E' as u16,
                ctrl: true,
                alt: false,
                shift: false,
            }
        );
        assert_eq!(
            find_box,
            ShortcutSpec {
                vk: b'S' as u16,
                ctrl: false,
                alt: true,
                shift: false,
            }
        );
        let unbound = shortcuts.replace("<virtualkey>2147483731</virtualkey>", "");
        assert!(parse_preset_search_shortcuts(&unbound)
            .unwrap_err()
            .contains("Select Find Box"));
        let unbound = shortcuts.replace("<virtualkey>2147483717</virtualkey>", "");
        assert!(parse_preset_search_shortcuts(&unbound)
            .unwrap_err()
            .contains("Effects"));
    }
}
