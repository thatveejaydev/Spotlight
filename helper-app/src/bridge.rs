use crate::catalog::{EffectEntry, ProjectItemEntry};
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    io::ErrorKind,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, SystemTime},
};
use tungstenite::{accept, client, Message};

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

fn find_latest_premiere_prefs_file() -> Option<PathBuf> {
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

pub fn read_premiere_transition_prefs() -> (TransitionDurationPref, TransitionDurationPref) {
    let mut video_pref = TransitionDurationPref {
        value: 30.0,
        unit: "frames".to_string(),
    };
    let mut audio_pref = TransitionDurationPref {
        value: 1.0,
        unit: "seconds".to_string(),
    };

    let Some(prefs_path) = find_latest_premiere_prefs_file() else {
        return (video_pref, audio_pref);
    };
    let Ok(xml) = fs::read_to_string(prefs_path) else {
        return (video_pref, audio_pref);
    };

    // Video Transition Default Duration:
    // BE.Prefs.DefaultVideoTransitionDuration is stored in frames when unit == 0 (or seconds if DefaultVideoTransitionDurationInSeconds is present)
    let v_unit_code = extract_xml_tag_value(&xml, "BE.Prefs.DefaultVideoTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if v_unit_code == 1 {
        if let Some(sec) = extract_xml_tag_value(&xml, "BE.Prefs.DefaultVideoTransitionDurationInSeconds")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            video_pref = TransitionDurationPref {
                value: sec,
                unit: "seconds".to_string(),
            };
        } else if let Some(val) = extract_xml_tag_value(&xml, "BE.Prefs.DefaultVideoTransitionDuration")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            // Premiere stores DefaultVideoTransitionDuration as frames even when the UI displays Seconds
            video_pref = TransitionDurationPref {
                value: val,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(frames) = extract_xml_tag_value(&xml, "BE.Prefs.DefaultVideoTransitionDuration")
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
    let a_unit_code = extract_xml_tag_value(&xml, "BE.Prefs.DefaultAudioTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if a_unit_code == 0 {
        if let Some(frames) = extract_xml_tag_value(&xml, "BE.Prefs.DefaultAudioTransitionDurationInFrames")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            audio_pref = TransitionDurationPref {
                value: frames,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(sec) = extract_xml_tag_value(&xml, "BE.Prefs.DefaultAudioTransitionDurationInSeconds")
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

pub enum BridgeEvent {
    ClientConnected,
    ClientDisconnected,
    CatalogReceived(Vec<EffectEntry>),
    ApplyResult(ApplyResult),
    ProjectItemsChanged,
    PlacementResult(PlacementResult),
    SourceMonitorResult(PlacementResult),
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
}

struct Peer {
    id: usize,
    tx: Sender<String>,
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

impl BridgeClients {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub(crate) fn connect_test_peer(&self, catalog: MediaCatalog) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        self.peers.lock().unwrap().push(Peer {
            id: catalog.peer_id,
            tx,
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
        for peer in self.peers.lock().unwrap().iter_mut() {
            peer.requested = request_id;
            if let Some(media) = &mut peer.media {
                media.items.clear();
            }
            let _ = peer.tx.send(json.clone());
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
        Ok(request_id)
    }

    pub fn open_in_source_monitor(
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
            return Err("Close the other Premiere session before opening media.");
        }
        let current = peer.media.as_ref().unwrap();
        if peer.id != catalog.peer_id || current.project_id != catalog.project_id {
            return Err("Project media changed. Search again.");
        }
        let entry = current.items.iter().find(|entry| entry.item_id == item_id)
            .ok_or("Project media changed. Search again.")?;
        if let Some(message) = entry.source_monitor_error() {
            return Err(message);
        }
        let project_id = current
            .project_id
            .clone()
            .ok_or("Open a Premiere project first.")?;
        let request_id = self.next_id.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        let msg = OutgoingHelperMessage::OpenInSourceMonitor {
            request_id,
            project_id,
            item_id,
        };
        peer.tx
            .send(serde_json::to_string(&msg).unwrap())
            .map_err(|_| "Premiere media bridge disconnected.")?;
        Ok(request_id)
    }

    pub fn send_apply(&self, entry: &EffectEntry) -> Option<(u64, Vec<usize>)> {
        let (video_transition_duration, audio_transition_duration) =
            read_premiere_transition_prefs();

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
        (!recipients.is_empty()).then_some((request_id, recipients))
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

    // Switch to a short read timeout so outbound messages are flushed within <=25ms
    let _ = ws
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(25)));

    let (tx, rx) = mpsc::channel::<String>();
    let peer_id = bridge.next_id.fetch_add(1, Ordering::Relaxed);
    let mut is_plugin_client = false;

    // Ask plugin to send its catalog right away
    let _ = ws.send(Message::Text(r#"{"type":"request_catalog"}"#.to_string()));
    let _ = ws.flush();

    loop {
        // 1. Drain any outbound commands queued from the UI thread
        let mut disconnected = false;
        while let Ok(outbound_json) = rx.try_recv() {
            if ws.send(Message::Text(outbound_json)).is_err() || ws.flush().is_err() {
                disconnected = true;
                break;
            }
        }
        if disconnected {
            break;
        }

        // 2. Read incoming messages from the UXP plugin (or wake-up ping)
        match ws.read() {
            Ok(Message::Text(txt)) => {
                if let Ok(parsed) = serde_json::from_str::<IncomingPluginMessage>(&txt) {
                    if !is_plugin_client
                        && !matches!(
                            parsed,
                            IncomingPluginMessage::ShowWindow
                                | IncomingPluginMessage::Ack { .. }
                                | IncomingPluginMessage::PlacementAck { .. }
                                | IncomingPluginMessage::SourceMonitorAck { .. }
                        )
                    {
                        is_plugin_client = true;
                        bridge.peers.lock().unwrap().push(Peer {
                            id: peer_id,
                            tx: tx.clone(),
                            media: None,
                            requested: 0,
                        });
                        cb(BridgeEvent::ClientConnected);
                    }
                    match parsed {
                        IncomingPluginMessage::Catalog { effects } => {
                            cb(BridgeEvent::CatalogReceived(effects));
                        }
                        IncomingPluginMessage::Ack { request_id, ok, message } => {
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
                            let mut peers = bridge.peers.lock().unwrap();
                            let peer = peers.iter_mut().find(|peer| peer.id == peer_id).unwrap();
                            if peer.accept_catalog(request_id, project_id, sequence_id, items) {
                                cb(BridgeEvent::ProjectItemsChanged);
                            }
                        }
                        IncomingPluginMessage::PlacementAck {
                            request_id,
                            ok,
                            message,
                        } => {
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
                            if is_plugin_client {
                                cb(BridgeEvent::SourceMonitorResult(PlacementResult {
                                    peer_id,
                                    request_id,
                                    ok,
                                    message,
                                }));
                            }
                        }
                        IncomingPluginMessage::Ping => {}
                        IncomingPluginMessage::ShowWindow => {
                            cb(BridgeEvent::ShowWindowRequested);
                            break;
                        }
                    }
                }
            }
            Ok(Message::Ping(payload)) => {
                let _ = ws.send(Message::Pong(payload));
                let _ = ws.flush();
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut =>
            {
                // Normal poll timeout; loop around to check outbound queue
            }
            Err(_) => break,
        }
    }

    if is_plugin_client {
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
        bridge.peers.lock().unwrap().push(Peer { id: 7, tx, media: None, requested: 0 });
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
            tx,
            media: None,
            requested: request_id,
        };
        assert!(peer.accept_catalog(request_id, project_id, sequence_id, items));
        assert!(!peer.accept_catalog(request_id - 1, None, None, vec![]));
        bridge.peers.lock().unwrap().push(peer);
        let catalog = bridge.media_catalog().unwrap().unwrap();
        for (item_id, message) in [
            ("b", "Bins can't be placed on the timeline."),
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
            tx,
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
}
