pub mod markers;

use serde::Serialize;
use std::time::{Duration, Instant};

/// How long Spotlight waits for Premiere to answer a command.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    AddAdjustmentLayer,
    AddMarker,
    GoToMarker,
    LockHorizontalAxis,
    RegisterAdjustmentLayer,
    RemoveAttributes,
}

impl Action {
    pub const ALL: [Self; 6] = [
        Self::AddAdjustmentLayer,
        Self::AddMarker,
        Self::GoToMarker,
        Self::LockHorizontalAxis,
        Self::RegisterAdjustmentLayer,
        Self::RemoveAttributes,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::AddAdjustmentLayer => "add_adjustment_layer",
            Self::AddMarker => "add_marker",
            Self::GoToMarker => "go_to_marker",
            Self::LockHorizontalAxis => "lock_horizontal_axis",
            Self::RegisterAdjustmentLayer => "register_adjustment_layer",
            Self::RemoveAttributes => "remove_attributes",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.id() == id)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::AddAdjustmentLayer => "Add Adjustment Layer",
            Self::AddMarker => "Add Marker",
            Self::GoToMarker => "Go to Marker",
            Self::LockHorizontalAxis => "Lock Horizontal Axis",
            Self::RegisterAdjustmentLayer => "Register Adjustment Layer",
            Self::RemoveAttributes => "Remove Attributes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::AddAdjustmentLayer => "Place a layer above selected video clips",
            Self::AddMarker => "Open the Marker dialog at the playhead",
            Self::GoToMarker => "Find a marker in the active project",
            Self::LockHorizontalAxis => "Hold hotkey over a handle to click-drag horizontally",
            Self::RegisterAdjustmentLayer => "Use the selected adjustment layer as this project's source",
            Self::RemoveAttributes => "Remove applied effects from selected clips",
        }
    }

    pub fn is_hold(self) -> bool {
        matches!(self, Self::LockHorizontalAxis)
    }

    pub fn hotkey_hint(self) -> &'static str {
        if self.is_hold() {
            "Hold while dragging in Premiere"
        } else {
            ""
        }
    }
}

pub struct ActionResult {
    pub peer_id: usize,
    pub request_id: u64,
    pub ok: bool,
    pub message: String,
}

pub struct PendingAction {
    pub peer_id: usize,
    pub request_id: u64,
    pub deadline: Instant,
}

impl PendingAction {
    pub fn outcome(&self, result: Option<ActionResult>) -> Option<Result<String, String>> {
        if let Some(result) = result {
            if result.peer_id == self.peer_id && result.request_id == self.request_id {
                return Some(if result.ok {
                    Ok(result.message)
                } else {
                    Err(result.message)
                });
            }
        }
        (Instant::now() >= self.deadline)
            .then(|| Err("Premiere did not report the action outcome. Check the timeline.".into()))
    }
}
