pub mod markers;

use serde::Serialize;

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
