pub mod markers;

use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    AddAdjustmentLayer,
    AddMarker,
    GoToMarker,
    RegisterAdjustmentLayer,
    RemoveAttributes,
}

impl Action {
    pub const ALL: [Self; 5] = [
        Self::AddAdjustmentLayer,
        Self::AddMarker,
        Self::GoToMarker,
        Self::RegisterAdjustmentLayer,
        Self::RemoveAttributes,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::AddAdjustmentLayer => "add_adjustment_layer",
            Self::AddMarker => "add_marker",
            Self::GoToMarker => "go_to_marker",
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
            Self::RegisterAdjustmentLayer => "Register Adjustment Layer",
            Self::RemoveAttributes => "Remove Attributes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::AddAdjustmentLayer => "Place a layer above selected video clips",
            Self::AddMarker => "Open the Marker dialog at the playhead",
            Self::GoToMarker => "Find a marker in the active project",
            Self::RegisterAdjustmentLayer => "Use the selected adjustment layer as this project's source",
            Self::RemoveAttributes => "Remove applied effects from selected clips",
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
