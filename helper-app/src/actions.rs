pub mod markers;

use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    AddMarker,
    GoToMarker,
    RemoveAttributes,
}

impl Action {
    pub const ALL: [Self; 3] = [Self::AddMarker, Self::GoToMarker, Self::RemoveAttributes];

    pub fn name(self) -> &'static str {
        match self {
            Self::AddMarker => "Add Marker",
            Self::GoToMarker => "Go to Marker",
            Self::RemoveAttributes => "Remove Attributes",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::AddMarker => "Open the Marker dialog at the playhead",
            Self::GoToMarker => "Find a marker in the active project",
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
