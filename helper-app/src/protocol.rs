//! Messages exchanged with the UXP bridge plugin over the WebSocket, in the
//! JSON shapes `uxp-plugin/index.js` sends and expects.

use crate::{
    actions::Action,
    catalog::{EffectEntry, EffectType, ProjectItemEntry},
    premiere_prefs::TransitionDurationPref,
};
use serde::{Deserialize, Serialize};

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
        /// How the plugin produced this list ("rescanned" or "cleared"); diagnostics only.
        #[serde(default)]
        scan_mode: Option<String>,
        /// Time the plugin spent producing this list, in milliseconds; diagnostics only.
        #[serde(default)]
        scan_ms: Option<u64>,
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
        effect_type: EffectType,
        #[serde(rename = "videoTransitionDuration")]
        video_transition_duration: TransitionDurationPref,
        #[serde(rename = "audioTransitionDuration")]
        audio_transition_duration: TransitionDurationPref,
    },
}


#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioPreviewSource {
    pub path: std::path::PathBuf,
    pub in_seconds: Option<f64>,
    pub out_seconds: Option<f64>,
}

// ---------------------------------------------------------------------------
// Sequence markers
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceRef {
    project_id: String,
    sequence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceMarker {
    pub(crate) sequence_id: String,
    pub sequence_name: String,
    pub name: String,
    pub comments: String,
    ticks: String,
}

impl SequenceMarker {
    pub fn title(&self) -> &str {
        if self.name.trim().is_empty() {
            "Unnamed marker"
        } else {
            &self.name
        }
    }

    pub(crate) fn ticks(&self) -> i128 {
        self.ticks.parse().unwrap_or_default()
    }

    pub fn position(&self) -> String {
        const TICKS_PER_MILLISECOND: u128 = 254_016_000;
        let ticks = self.ticks();
        let millis = ticks.unsigned_abs() / TICKS_PER_MILLISECOND;
        let seconds = millis / 1000;
        let sign = if ticks < 0 { "-" } else { "" };
        format!(
            "{sign}{:02}:{:02}:{:02}.{:03}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            millis % 1000
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MarkerLocation {
    sequence: SequenceRef,
    ticks: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum MarkerCommand {
    List,
    Seek {
        #[serde(rename = "projectId")]
        project_id: String,
        marker: SequenceMarker,
    },
    Prepare,
    Confirm {
        location: MarkerLocation,
    },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MarkerReply {
    List {
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(default, rename = "projectName")]
        project_name: String,
        markers: Vec<SequenceMarker>,
    },
    Sought,
    Prepared {
        location: MarkerLocation,
        created: bool,
    },
    Confirmed,
}

