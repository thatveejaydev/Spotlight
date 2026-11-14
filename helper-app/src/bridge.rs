use crate::catalog::EffectEntry;
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
    #[serde(rename = "apply_effect")]
    ApplyEffect {
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
    ApplyResult { ok: bool, message: String },
    ShowWindowRequested,
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
    peers: Arc<Mutex<Vec<Sender<String>>>>,
    active_count: Arc<AtomicUsize>,
}

impl BridgeClients {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(Vec::new())),
            active_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn send_apply(&self, entry: &EffectEntry) -> bool {
        let (video_transition_duration, audio_transition_duration) =
            read_premiere_transition_prefs();

        let msg = OutgoingHelperMessage::ApplyEffect {
            name: entry.name.clone(),
            match_name: entry.match_name.clone(),
            effect_type: entry.effect_type.clone(),
            video_transition_duration,
            audio_transition_duration,
        };
        let Ok(json) = serde_json::to_string(&msg) else {
            return false;
        };

        let mut peers = self.peers.lock().unwrap();
        let mut sent_any = false;
        peers.retain(|tx| match tx.send(json.clone()) {
            Ok(()) => {
                sent_any = true;
                true
            }
            Err(_) => false,
        });
        sent_any
    }

    pub fn start_server<F>(&self, on_event: F)
    where
        F: Fn(BridgeEvent) + Send + Sync + 'static,
    {
        let callback = Arc::new(on_event);

        for addr in BRIDGE_ADDRS {
            let peers = self.peers.clone();
            let active_count = self.active_count.clone();
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
                    let peers_ref = peers.clone();
                    let count_ref = active_count.clone();
                    let cb_ref = cb.clone();

                    thread::spawn(move || {
                        handle_client(stream, peers_ref, count_ref, cb_ref);
                    });
                }
            });
        }
    }
}

fn handle_client<F>(
    stream: TcpStream,
    peers: Arc<Mutex<Vec<Sender<String>>>>,
    active_count: Arc<AtomicUsize>,
    cb: Arc<F>,
) where
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
                    if !is_plugin_client && !matches!(parsed, IncomingPluginMessage::ShowWindow) {
                        is_plugin_client = true;
                        peers.lock().unwrap().push(tx.clone());
                        active_count.fetch_add(1, Ordering::SeqCst);
                        cb(BridgeEvent::ClientConnected);
                    }
                    match parsed {
                        IncomingPluginMessage::Catalog { effects } => {
                            cb(BridgeEvent::CatalogReceived(effects));
                        }
                        IncomingPluginMessage::Ack { ok, message } => {
                            cb(BridgeEvent::ApplyResult { ok, message });
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
        let remaining = active_count.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        if remaining == 0 {
            cb(BridgeEvent::ClientDisconnected);
        }
    }
}
