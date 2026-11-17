use crate::actions::Action;
use crate::catalog::{EffectEntry, ProjectItemEntry, ProjectItemKind};
use crate::debug_log::{self, LogKind};
use crate::premiere_prefs::{default_transition_prefs, read_premiere_transition_prefs};
use crate::protocol::{
    AudioPreviewSource, IncomingPluginMessage, MarkerCommand, MarkerReply, OutgoingHelperMessage,
    PremiereContext,
};
use crate::request::{CommandReply, PendingRequest};
use std::{
    collections::HashMap,
    io::ErrorKind,
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::{
    accept_hdr, client,
    handshake::server::{ErrorResponse, Request, Response},
    Message,
};

/// Browsers attach an `http(s)` Origin to every WebSocket handshake, so a web page
/// could otherwise connect to the bridge. Premiere's UXP plugin and a second
/// Spotlight instance send no Origin or a non-web one, and local pages stay allowed.
fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin.map(str::trim).filter(|origin| !origin.is_empty()) else {
        return true;
    };
    let lower = origin.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("http://")
        .or_else(|| lower.strip_prefix("https://"))
    else {
        // Non-web schemes (and the opaque "null") are kept so a UXP host origin can never lock the plugin out.
        return true;
    };
    let host = if let Some(bracketed) = rest.strip_prefix('[') {
        bracketed.split(']').next().unwrap_or_default()
    } else {
        rest.split([':', '/']).next().unwrap_or_default()
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

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

pub enum BridgeEvent {
    ClientConnected,
    ClientDisconnected,
    CatalogReceived(Vec<EffectEntry>),
    ApplyResult(ApplyResult),
    ActionResult(CommandReply),
    MarkerResult(MarkerEvent),
    ProjectItemsChanged,
    PlacementResult(CommandReply),
    /// The reply to opening an item in the Source Monitor or a sequence in the Timeline.
    OpenResult(CommandReply),
    AudioPreviewResult(AudioPreviewResult),
    ContextReceived(ContextEvent),
    ShowWindowRequested,
}

/// The plugin's reply to a context request; `context` is `None` when it couldn't read it.
pub struct ContextEvent {
    pub peer_id: usize,
    pub request_id: u64,
    pub context: Option<PremiereContext>,
}

pub struct ApplyResult {
    pub peer_id: usize,
    pub request_id: Option<u64>,
    pub ok: bool,
    pub message: String,
}

pub struct AudioPreviewResult {
    pub peer_id: usize,
    pub request_id: u64,
    pub source: Option<AudioPreviewSource>,
    pub message: String,
}

/// The plugin's reply to a marker command.
pub struct MarkerEvent {
    pub peer_id: usize,
    pub request_id: u64,
    pub result: Option<MarkerReply>,
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
    /// True from a project-items request until the plugin sends a finished list.
    scanning: bool,
}

#[derive(Clone)]
pub struct MediaCatalog {
    pub peer_id: usize,
    pub project_id: Option<String>,
    pub sequence_id: Option<String>,
    pub items: Vec<ProjectItemEntry>,
}

/// The error texts for a command when no plugin, or more than one, is connected.
struct PeerErrors {
    disconnected: &'static str,
    multiple: &'static str,
}

const MARKER_PEERS: PeerErrors = PeerErrors {
    disconnected: "The Premiere marker bridge disconnected.",
    multiple: "Leave one Premiere bridge connected to use sequence markers.",
};
const ACTION_PEERS: PeerErrors = PeerErrors {
    disconnected: "Premiere action bridge disconnected.",
    multiple: "Multiple Premiere bridges are connected. Leave one connected to run an action.",
};
const MEDIA_DISCONNECTED: &str = "Premiere media bridge disconnected.";
const MEDIA_CHANGED: &str = "Project media changed. Search again.";

/// The only connected plugin, which must be `expected` when one is given.
fn sole_peer<'a>(
    peers: &'a [Peer],
    expected: Option<usize>,
    errors: &PeerErrors,
) -> Result<&'a Peer, &'static str> {
    match peers {
        [peer] if expected.is_none_or(|id| id == peer.id) => Ok(peer),
        [] | [_] => Err(errors.disconnected),
        _ => Err(errors.multiple),
    }
}

/// Finds `item_id` in the only plugin's project list, which must still be the
/// list `catalog` came from (and the same sequence when `same_sequence`).
fn project_source<'a>(
    peers: &'a [Peer],
    catalog: &MediaCatalog,
    item_id: &str,
    same_sequence: bool,
    multiple: &'static str,
) -> Result<(&'a Peer, &'a MediaCatalog, &'a ProjectItemEntry), &'static str> {
    let mut catalogs = peers.iter().filter_map(|peer| Some((peer, peer.media.as_ref()?)));
    let (peer, current) = catalogs.next().ok_or(MEDIA_DISCONNECTED)?;
    if catalogs.next().is_some() {
        return Err(multiple);
    }
    if peer.id != catalog.peer_id
        || current.project_id != catalog.project_id
        || (same_sequence && current.sequence_id != catalog.sequence_id)
    {
        return Err(MEDIA_CHANGED);
    }
    let entry = current.items.iter().find(|entry| entry.item_id == item_id).ok_or(MEDIA_CHANGED)?;
    Ok((peer, current, entry))
}

fn require_project(current: &MediaCatalog) -> Result<&str, &'static str> {
    current.project_id.as_deref().ok_or("Open a Premiere project first.")
}

impl BridgeClients {
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the peer list, recovering it if another thread panicked while
    /// holding it. Release builds abort on panic, so a poisoned unwrap here
    /// would close the app with no message.
    fn peers(&self) -> MutexGuard<'_, Vec<Peer>> {
        self.peers.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Request IDs and peer IDs share one counter, so every ID is unique.
    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1
    }

    /// Sends one command to `peer` and starts timing its round trip.
    fn send_command(
        &self,
        peer: &Peer,
        request_id: u64,
        label: &'static str,
        message: &OutgoingHelperMessage,
        disconnected: &'static str,
    ) -> Result<(), &'static str> {
        let json = serde_json::to_string(message).map_err(|_| "Could not prepare the command for Premiere.")?;
        peer.tx.send(json).map_err(|_| disconnected)?;
        self.track_request(request_id, label);
        Ok(())
    }

    fn log_sent(label: &str, detail: &str, request_id: u64) {
        debug_log::log_event(LogKind::Bridge, "BRIDGE", format!("-> {label}{detail} (id={request_id})"));
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
        self.peers().push(Peer {
            id: catalog.peer_id,
            tx: PeerSender::from_sender(tx),
            media: Some(catalog),
            requested: 0,
            scanning: false,
        });
        rx
    }

    /// True while any plugin is still building the project list.
    pub fn project_scan_pending(&self) -> bool {
        self.peers().iter().any(|peer| peer.scanning)
    }

    pub fn media_catalog(&self) -> Result<Option<MediaCatalog>, &'static str> {
        let peers = self.peers();
        let mut catalogs = peers.iter().filter_map(|peer| peer.media.as_ref());
        let first = catalogs.next();
        if catalogs.next().is_some() {
            return Err("Multiple Premiere sessions. Close the other session to place media.");
        }
        Ok(first.cloned())
    }

    pub fn request_project_items(&self) {
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::RequestProjectItems { request_id };
        let json = serde_json::to_string(&msg).unwrap();
        let mut sent = 0usize;
        // Keep the last list so the palette shows it while the plugin checks for changes.
        for peer in self.peers().iter_mut() {
            peer.requested = request_id;
            if peer.tx.send(json.clone()).is_ok() {
                peer.scanning = true;
                sent += 1;
            }
        }
        if sent > 0 {
            self.track_request(request_id, "project_items");
            Self::log_sent("request_project_items", "", request_id);
        }
    }

    pub fn place_project_item(
        &self,
        catalog: &MediaCatalog,
        item_id: String,
    ) -> Result<u64, &'static str> {
        let peers = self.peers();
        let (peer, current, entry) = project_source(
            &peers,
            catalog,
            &item_id,
            true,
            "Close the other Premiere session before placing media.",
        )?;
        if let Some(message) = entry.placement_error() {
            return Err(message);
        }
        let project_id = require_project(current)?.to_owned();
        let sequence_id = current
            .sequence_id
            .clone()
            .ok_or("Open a timeline before placing media.")?;
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::PlaceProjectItem { request_id, project_id, sequence_id, item_id };
        self.send_command(peer, request_id, "place_project_item", &msg, MEDIA_DISCONNECTED)?;
        Self::log_sent("place_project_item", "", request_id);
        Ok(request_id)
    }

    pub fn open_in_source_monitor(
        &self,
        catalog: &MediaCatalog,
        item_id: String,
    ) -> Result<u64, &'static str> {
        let peers = self.peers();
        let (peer, current, entry) = project_source(
            &peers,
            catalog,
            &item_id,
            false,
            "Close the other Premiere session before opening media.",
        )?;
        let project_id = require_project(current)?.to_owned();
        if let Some(message) = entry.source_monitor_error() {
            return Err(message);
        }
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::OpenInSourceMonitor { request_id, project_id, item_id };
        self.send_command(peer, request_id, "open_in_source_monitor", &msg, MEDIA_DISCONNECTED)?;
        Self::log_sent("open_in_source_monitor", "", request_id);
        Ok(request_id)
    }

    /// Opens a project sequence in its own Timeline tab.
    pub fn open_sequence(&self, catalog: &MediaCatalog, item_id: String) -> Result<u64, &'static str> {
        let peers = self.peers();
        let (peer, current, entry) = project_source(
            &peers,
            catalog,
            &item_id,
            false,
            "Close the other Premiere session before opening sequences.",
        )?;
        let project_id = require_project(current)?.to_owned();
        if !matches!(entry.kind, ProjectItemKind::Sequence | ProjectItemKind::ActiveSequence) {
            return Err("Only sequences open in the Timeline.");
        }
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::OpenSequence { request_id, project_id, item_id };
        self.send_command(peer, request_id, "open_sequence", &msg, MEDIA_DISCONNECTED)?;
        Self::log_sent("open_sequence", "", request_id);
        Ok(request_id)
    }

    pub fn request_audio_preview(
        &self,
        catalog: &MediaCatalog,
        item_id: &str,
    ) -> Result<u64, &'static str> {
        let peers = self.peers();
        let (peer, current, entry) = project_source(
            &peers,
            catalog,
            item_id,
            false,
            "Close the other Premiere session before opening media.",
        )?;
        let project_id = require_project(current)?.to_owned();
        if entry.kind != crate::catalog::ProjectItemKind::Media {
            return Err("Select an audio clip to preview.");
        }
        if entry.offline {
            return Err("Relink the offline media before previewing it.");
        }
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::RequestAudioPreview {
            request_id,
            project_id,
            item_id: item_id.to_owned(),
        };
        // Not logged: auto-preview sends one per highlighted clip.
        self.send_command(peer, request_id, "request_audio_preview", &msg, MEDIA_DISCONNECTED)?;
        Ok(request_id)
    }

    pub fn marker_peer_id(&self) -> Result<usize, &'static str> {
        let peers = self.peers();
        sole_peer(&peers, None, &MARKER_PEERS).map(|peer| peer.id)
    }

    pub fn marker_command(
        &self,
        command: MarkerCommand,
        expected_peer: Option<usize>,
    ) -> Result<PendingRequest, &'static str> {
        let peers = self.peers();
        let peer = sole_peer(&peers, expected_peer, &MARKER_PEERS)?;
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::MarkerCommand { request_id, command };
        self.send_command(peer, request_id, "marker_command", &msg, MARKER_PEERS.disconnected)?;
        Ok(PendingRequest::new(peer.id, request_id))
    }

    /// Asks the only connected plugin for the Premiere state, returning the request
    /// and peer IDs. With no plugin, or more than one, the state stays unknown.
    pub fn request_context(&self) -> Option<(u64, usize)> {
        let peers = self.peers();
        let [peer] = peers.as_slice() else {
            return None;
        };
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::RequestContext { request_id };
        self.send_command(peer, request_id, "context", &msg, "").ok()?;
        Self::log_sent("request_context", "", request_id);
        Some((request_id, peer.id))
    }

    pub fn run_action(&self, action: Action) -> Result<(u64, usize), &'static str> {
        let peers = self.peers();
        let peer = sole_peer(&peers, None, &ACTION_PEERS)?;
        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::RunAction { request_id, action };
        self.send_command(peer, request_id, "run_action", &msg, ACTION_PEERS.disconnected)?;
        Self::log_sent("run_action", &format!(" \"{}\"", action.name()), request_id);
        Ok((request_id, peer.id))
    }

    pub fn send_apply(&self, entry: &EffectEntry) -> Option<(u64, Vec<usize>)> {
        if entry.is_preset() {
            return None;
        }
        let (video_transition_duration, audio_transition_duration) =
            if entry.effect_type.is_transition() {
                read_premiere_transition_prefs()
            } else {
                default_transition_prefs()
            };

        let request_id = self.next_request_id();
        let msg = OutgoingHelperMessage::ApplyEffect {
            request_id,
            name: entry.name.clone(),
            match_name: entry.match_name.clone(),
            effect_type: entry.effect_type,
            video_transition_duration,
            audio_transition_duration,
        };
        let Ok(json) = serde_json::to_string(&msg) else {
            return None;
        };

        let peers = self.peers();
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

    #[allow(clippy::result_large_err)] // tungstenite fixes the callback signature.
    let check_origin = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let origin = request
            .headers()
            .get("origin")
            .map(|value| value.to_str().unwrap_or("<invalid>"));
        if origin_allowed(origin) {
            return Ok(response);
        }
        debug_log::log_event(
            LogKind::Error,
            "BRIDGE",
            format!("Rejected WebSocket client from origin {}", origin.unwrap_or_default()),
        );
        let mut rejection = ErrorResponse::new(Some("Origin not allowed".into()));
        *rejection.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
        Err(rejection)
    };
    let Ok(mut ws) = accept_hdr(stream, check_origin) else {
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
                                    | IncomingPluginMessage::OpenSequenceAck { .. }
                                    | IncomingPluginMessage::AudioPreviewAck { .. }
                            )
                        {
                            is_plugin_client = true;
                            bridge.peers().push(Peer {
                                id: peer_id,
                                tx: peer_tx.clone(),
                                media: None,
                                requested: 0,
                                scanning: false,
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
                                scan_mode,
                                scan_ms,
                            } => {
                                let item_count = items.len();
                                let scan = match (scan_mode.as_deref(), scan_ms) {
                                    (Some(mode), Some(ms)) => format!(", {mode} in {ms} ms"),
                                    (Some(mode), None) => format!(", {mode}"),
                                    _ => String::new(),
                                };
                                let mut peers = bridge.peers();
                                let Some(peer) = peers.iter_mut().find(|peer| peer.id == peer_id) else {
                                    continue;
                                };
                                if peer.accept_catalog(request_id, project_id, sequence_id, items) {
                                    // "scanning" is the empty placeholder sent before a full rescan.
                                    peer.scanning = scan_mode.as_deref() == Some("scanning");
                                    drop(peers);
                                    bridge.finish_request(request_id);
                                    debug_log::log_event(
                                        LogKind::Bridge,
                                        "BRIDGE",
                                        format!("<- project_items (id={request_id}, {item_count} items{scan})"),
                                    );
                                    cb(BridgeEvent::ProjectItemsChanged);
                                }
                            }
                            IncomingPluginMessage::ActionAck { request_id, ok, message } => {
                                if is_plugin_client {
                                    bridge.finish_request(request_id);
                                    cb(BridgeEvent::ActionResult(CommandReply {
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
                                    cb(BridgeEvent::PlacementResult(CommandReply {
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
                            }
                            | IncomingPluginMessage::OpenSequenceAck {
                                request_id,
                                ok,
                                message,
                            } => {
                                bridge.finish_request(request_id);
                                if is_plugin_client {
                                    cb(BridgeEvent::OpenResult(CommandReply {
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
                            IncomingPluginMessage::Context { request_id, context, elapsed_ms } => {
                                bridge.finish_request(request_id);
                                if is_plugin_client {
                                    let selection = context.as_ref().and_then(|context| context.selection.as_ref());
                                    let read = elapsed_ms.map(|ms| format!(", read in {ms} ms")).unwrap_or_default();
                                    debug_log::log_event(
                                        LogKind::Bridge,
                                        "BRIDGE",
                                        format!("<- context (id={request_id}{read}) selection={selection:?}"),
                                    );
                                    cb(BridgeEvent::ContextReceived(ContextEvent {
                                        peer_id, request_id, context,
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
        let mut peers = bridge.peers();
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
        bridge.peers().push(Peer {
            id: 7,
            tx: PeerSender::from_sender(tx),
            media: None,
            requested: 0,
            scanning: false,
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
        assert_eq!(command["effectType"], "videoEffect");
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
        let IncomingPluginMessage::ProjectItems { request_id, project_id, sequence_id, items, .. } =
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
            scanning: false,
        };
        assert!(peer.accept_catalog(request_id, project_id, sequence_id, items));
        assert!(!peer.accept_catalog(request_id - 1, None, None, vec![]));
        bridge.peers().push(peer);
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
        bridge.peers().push(Peer {
            id: 43,
            tx: PeerSender::from_sender(tx),
            media: Some(MediaCatalog {
                peer_id: 43,
                project_id: None,
                sequence_id: None,
                items: Vec::new(),
            }),
            requested: 0,
            scanning: false,
        });
        assert!(bridge.media_catalog().is_err());
        assert!(bridge.place_project_item(&catalog, "i".into()).is_err());
        assert!(rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
        bridge.peers().pop();

        bridge.request_project_items();
        let message: serde_json::Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(message["type"], "request_project_items");
        assert!(bridge.project_scan_pending());
        // The last list stays usable while the plugin checks for changes.
        assert_eq!(bridge.media_catalog().unwrap().unwrap().items.len(), 5);
        assert!(bridge.place_project_item(&catalog, "i".into()).is_ok());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&rx.try_recv().unwrap()).unwrap()["type"],
            "place_project_item"
        );
    }

    #[test]
    fn project_items_message_reports_scan_mode() {
        let IncomingPluginMessage::ProjectItems { scan_mode, scan_ms, .. } = serde_json::from_str(
            r#"{"type":"project_items","requestId":1,"projectId":null,"sequenceId":null,"items":[],"scanMode":"reused","scanMs":2}"#,
        )
        .unwrap() else {
            panic!("expected a media catalog");
        };
        assert_eq!((scan_mode.as_deref(), scan_ms), (Some("reused"), Some(2)));
        let IncomingPluginMessage::ProjectItems { scan_mode, scan_ms, .. } = serde_json::from_str(
            r#"{"type":"project_items","requestId":1,"projectId":null,"sequenceId":null,"items":[]}"#,
        )
        .unwrap() else {
            panic!("expected a media catalog");
        };
        assert_eq!((scan_mode, scan_ms), (None, None));
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
        let Message::Text(text) = msg else { panic!("expected a text command") };
        assert!(text.contains("request_project_items"));
        assert!(
            rtt < Duration::from_millis(200),
            "expected fast event-driven wakeup, got {rtt:?}"
        );

        // The scan stays pending through the "scanning" placeholder and ends with the full list.
        assert!(bridge.project_scan_pending());
        let request_id = serde_json::from_str::<serde_json::Value>(&text).unwrap()["requestId"]
            .as_u64()
            .unwrap();
        let wait_for = |pending: bool| {
            let deadline = Instant::now() + Duration::from_secs(2);
            while bridge.project_scan_pending() != pending {
                assert!(Instant::now() < deadline, "scan pending never became {pending}");
                thread::sleep(Duration::from_millis(5));
            }
        };
        for (mode, pending) in [("scanning", true), ("rescanned", false)] {
            ws.send(Message::Text(format!(
                r#"{{"type":"project_items","requestId":{request_id},"projectId":null,"sequenceId":null,"items":[],"scanMode":"{mode}","scanMs":0}}"#
            ))).unwrap();
            ws.flush().unwrap();
            wait_for(pending);
        }

        let _ = ws.close(None);
        server.join().unwrap();
    }

    #[test]
    fn bridge_rejects_web_page_origins_but_keeps_local_and_plugin_clients() {
        for origin in [None, Some(""), Some("null"), Some("uxp://plugin"), Some("file://"),
            Some("http://localhost:8080"), Some("https://127.0.0.1"), Some("http://[::1]:3000/")]
        {
            assert!(origin_allowed(origin), "{origin:?} should be allowed");
        }
        for origin in ["https://example.com", "http://localhost.example.com", "HTTP://evil.test:3847"] {
            assert!(!origin_allowed(Some(origin)), "{origin} should be rejected");
        }

        use tungstenite::client::IntoClientRequest;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let bridge = BridgeClients::new();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            handle_client(stream, bridge.clone(), Arc::new(|_event: BridgeEvent| {}));
            bridge.peers().len()
        });
        let mut request = format!("ws://{addr}").into_client_request().unwrap();
        request.headers_mut().insert("Origin", "https://example.com".parse().unwrap());
        assert!(client(request, TcpStream::connect(addr).unwrap()).is_err());
        assert_eq!(server.join().unwrap(), 0);
    }

    #[test]
    fn single_peer_commands_report_missing_or_extra_bridges() {
        let bridge = BridgeClients::new();
        assert_eq!(bridge.run_action(Action::RemoveAttributes), Err(ACTION_PEERS.disconnected));
        assert_eq!(bridge.marker_peer_id(), Err(MARKER_PEERS.disconnected));

        let catalog = |peer_id| MediaCatalog {
            peer_id,
            project_id: Some("p".into()),
            sequence_id: Some("s".into()),
            items: vec![],
        };
        let _first = bridge.connect_test_peer(catalog(1));
        assert_eq!(bridge.marker_peer_id(), Ok(1));
        assert!(matches!(
            bridge.marker_command(MarkerCommand::List, Some(2)),
            Err(message) if message == MARKER_PEERS.disconnected
        ));
        let (id, peer) = bridge.run_action(Action::RemoveAttributes).unwrap();
        assert_eq!(peer, 1);
        assert!(id > 0);

        let _second = bridge.connect_test_peer(catalog(2));
        assert_eq!(bridge.run_action(Action::RemoveAttributes), Err(ACTION_PEERS.multiple));
        assert_eq!(bridge.marker_peer_id(), Err(MARKER_PEERS.multiple));
        assert_eq!(
            bridge.place_project_item(&catalog(1), "i".into()),
            Err("Close the other Premiere session before placing media.")
        );
        assert_eq!(
            bridge.open_in_source_monitor(&catalog(1), "i".into()),
            Err("Close the other Premiere session before opening media.")
        );
    }
}
