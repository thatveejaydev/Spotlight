use crate::{
    bridge::{BridgeClients, MediaCatalog},
    premiere_prefs::{read_marker_shortcuts, ShortcutSpec},
    request::PendingRequest,
    win_focus,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceRef {
    project_id: String,
    sequence_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequenceMarker {
    sequence_id: String,
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

    fn ticks(&self) -> i128 {
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

pub struct MarkerEvent {
    pub peer_id: usize,
    pub request_id: u64,
    pub result: Option<MarkerReply>,
    pub message: String,
}

impl MarkerEvent {
    fn answers(&self, request: &PendingRequest) -> bool {
        request.is_answered_by(self.peer_id, self.request_id)
    }
}

pub struct MarkerPicker {
    pub previous_query: slint::SharedString,
    pub context_label: String,
    list: Option<(usize, String, Vec<Arc<SequenceMarker>>)>,
    request: Option<PendingRequest>,
    error: Option<String>,
}

impl MarkerPicker {
    pub fn open(bridge: &BridgeClients, previous_query: slint::SharedString) -> Self {
        let (request, error) = match bridge.marker_command(MarkerCommand::List, None) {
            Ok(request) => (Some(request), None),
            Err(message) => (None, Some(message.into())),
        };
        Self {
            previous_query,
            context_label: String::new(),
            list: None,
            request,
            error,
        }
    }

    pub fn markers(&self) -> &[Arc<SequenceMarker>] {
        self.list.as_ref().map_or(&[], |(_, _, markers)| markers)
    }

    pub fn status(&self) -> &str {
        if let Some(error) = &self.error {
            error
        } else if self.request.is_some() {
            if self.list.is_some() {
                "Moving playhead…"
            } else {
                "Loading markers…"
            }
        } else if self.markers().is_empty() {
            "No markers in this project"
        } else {
            ""
        }
    }

    pub fn refresh_if_project_changed(
        &mut self,
        bridge: &BridgeClients,
        catalog: &MediaCatalog,
    ) -> bool {
        let Some((peer, project, _)) = &self.list else {
            return false;
        };
        let Some(project_id) = &catalog.project_id else {
            return false;
        };
        if *peer == catalog.peer_id && project == project_id {
            return false;
        }
        *self = Self::open(bridge, self.previous_query.clone());
        true
    }

    pub fn seek(&mut self, bridge: &BridgeClients, marker: Arc<SequenceMarker>) {
        if self.request.is_some() {
            return;
        }
        let Some((peer_id, project_id, _)) = &self.list else {
            return;
        };
        let command = MarkerCommand::Seek {
            project_id: project_id.clone(),
            marker: (*marker).clone(),
        };
        self.error = None;
        match bridge.marker_command(command, Some(*peer_id)) {
            Ok(request) => self.request = Some(request),
            Err(message) => self.error = Some(message.into()),
        }
    }

    pub fn accepts(&self, event: &MarkerEvent) -> bool {
        self.request
            .as_ref()
            .is_some_and(|request| event.answers(request))
    }

    pub fn receive(&mut self, event: MarkerEvent) -> bool {
        self.request = None;
        match event.result {
            Some(MarkerReply::List {
                project_id,
                project_name,
                mut markers,
            }) if self.list.is_none() => {
                self.context_label = project_name;
                markers.sort_by(|a, b| {
                    a.sequence_name
                        .cmp(&b.sequence_name)
                        .then_with(|| a.sequence_id.cmp(&b.sequence_id))
                        .then_with(|| a.ticks().cmp(&b.ticks()))
                });
                self.list = Some((
                    event.peer_id,
                    project_id,
                    markers.into_iter().map(Arc::new).collect(),
                ));
            }
            Some(MarkerReply::Sought) if self.list.is_some() => return true,
            _ => self.error = Some(reply_error(event.message)),
        }
        false
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.request.as_ref().map(|request| request.deadline)
    }

    pub fn expire(&mut self) -> bool {
        if self
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.request = None;
            self.error = Some("Premiere did not respond. Open Go to Marker again.".into());
            return true;
        }
        false
    }
}

fn reply_error(message: String) -> String {
    if message.is_empty() {
        "Premiere returned an unexpected marker reply.".into()
    } else {
        message
    }
}

#[derive(Clone, Copy)]
struct DialogFocus {
    process_id: u32,
    started_at: u32,
}

impl DialogFocus {
    fn was_lost(self, process_id: Option<u32>, event_time: u32) -> bool {
        // WinEvent can deliver an event queued before the palette hid.
        (event_time.wrapping_sub(self.started_at) as i32) > 0 && process_id != Some(self.process_id)
    }
}

thread_local! {
    static DIALOG_FOCUS: Cell<Option<DialogFocus>> = const { Cell::new(None) };
}

#[cfg(windows)]
pub(crate) fn foreground_changed(hwnd: isize, event_time: u32) {
    if hwnd != 0
        && DIALOG_FOCUS
            .get()
            .is_some_and(|watch| watch.was_lost(win_focus::window_process_id(hwnd), event_time))
    {
        DIALOG_FOCUS.set(None);
    }
}

enum DialogStep {
    Focus {
        until: Instant,
        poll_at: Instant,
    },
    PrepareAt(Instant),
    Preparing(PendingRequest),
    Confirming(PendingRequest),
    Verify {
        owner: isize,
        until: Instant,
        poll_at: Instant,
    },
}

pub struct MarkerDialog {
    peer_id: usize,
    process_id: u32,
    timeline: ShortcutSpec,
    add_marker: ShortcutSpec,
    step: DialogStep,
    created: bool,
}

impl MarkerDialog {
    pub fn new(target: isize, bridge: &BridgeClients) -> Result<Self, &'static str> {
        let peer_id = bridge.marker_peer_id()?;
        let executable = win_focus::premiere_executable(target)
            .ok_or("Open Spotlight from Premiere to add a sequence marker.")?;
        let process_id =
            win_focus::window_process_id(target).ok_or("Premiere is no longer available.")?;
        let (timeline, add_marker) = read_marker_shortcuts(&executable)?;
        let now = Instant::now();
        Ok(Self {
            peer_id,
            process_id,
            timeline,
            add_marker,
            step: DialogStep::Focus {
                until: now + Duration::from_secs(2),
                poll_at: now,
            },
            created: false,
        })
    }

    pub fn deadline(&self) -> Instant {
        match &self.step {
            DialogStep::Focus { poll_at, .. } | DialogStep::Verify { poll_at, .. } => *poll_at,
            DialogStep::PrepareAt(at) => *at,
            DialogStep::Preparing(request) | DialogStep::Confirming(request) => request.deadline,
        }
    }

    pub fn accepts(&self, event: &MarkerEvent) -> bool {
        match &self.step {
            DialogStep::Preparing(request) | DialogStep::Confirming(request) => {
                event.answers(request)
            }
            _ => false,
        }
    }

    fn check_focus(&self) -> Result<(), String> {
        if DIALOG_FOCUS.get().is_some()
            && win_focus::window_process_id(win_focus::get_foreground_hwnd())
                == Some(self.process_id)
        {
            Ok(())
        } else {
            Err("Premiere lost focus.".into())
        }
    }

    pub fn receive(&mut self, bridge: &BridgeClients, event: MarkerEvent) -> Result<(), String> {
        match (&self.step, event.result) {
            (DialogStep::Preparing(_), Some(MarkerReply::Prepared { location, created })) => {
                self.created = created;
                self.check_focus()?;
                self.step = DialogStep::Confirming(
                    bridge
                        .marker_command(MarkerCommand::Confirm { location }, Some(event.peer_id))?,
                );
                Ok(())
            }
            (DialogStep::Confirming(_), Some(MarkerReply::Confirmed)) => {
                self.check_focus()?;
                let owner = win_focus::send_marker_shortcut(self.process_id, self.add_marker)
                    .ok_or("Could not send the Add Marker shortcut to Premiere.")?;
                let now = Instant::now();
                self.step = DialogStep::Verify {
                    owner,
                    until: now + Duration::from_secs(2),
                    poll_at: now,
                };
                Ok(())
            }
            _ => Err(reply_error(event.message)),
        }
    }

    pub fn tick(&mut self, bridge: &BridgeClients) -> Result<bool, String> {
        let now = Instant::now();
        if now < self.deadline() {
            return Ok(false);
        }
        match &mut self.step {
            DialogStep::Focus { until, poll_at } => {
                let started_at = win_focus::foreground_event_clock();
                if win_focus::send_marker_shortcut(self.process_id, self.timeline).is_some() {
                    DIALOG_FOCUS.set(Some(DialogFocus {
                        process_id: self.process_id,
                        started_at,
                    }));
                    self.step = DialogStep::PrepareAt(now + Duration::from_millis(160));
                } else if now >= *until {
                    return Err("Could not activate the Timeline in Premiere.".into());
                } else {
                    *poll_at = now + Duration::from_millis(20);
                }
            }
            DialogStep::PrepareAt(_) => {
                self.check_focus()?;
                self.step = DialogStep::Preparing(
                    bridge.marker_command(MarkerCommand::Prepare, Some(self.peer_id))?,
                );
            }
            DialogStep::Preparing(_) => {
                return Err(
                    "Premiere did not confirm the marker. Check the timeline before retrying."
                        .into(),
                );
            }
            DialogStep::Confirming(_) => {
                return Err("Premiere did not confirm the playhead position.".into())
            }
            DialogStep::Verify {
                owner,
                until,
                poll_at,
            } => {
                if win_focus::has_owned_modal(*owner) {
                    return Ok(true);
                }
                if win_focus::window_process_id(win_focus::get_foreground_hwnd())
                    != Some(self.process_id)
                    || now >= *until
                {
                    return Err("Could not confirm that Premiere opened the Marker dialog.".into());
                }
                *poll_at = now + Duration::from_millis(25);
            }
        }
        Ok(false)
    }

    pub fn failure(&self, message: String) -> String {
        if self.created {
            format!("Marker added. Open it in the timeline to edit it. {message}")
        } else {
            message
        }
    }
}

impl Drop for MarkerDialog {
    fn drop(&mut self) {
        DIALOG_FOCUS.set(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_watch_ignores_queued_events_and_premiere_window_changes() {
        let watch = DialogFocus {
            process_id: 42,
            started_at: 500,
        };
        assert!(!watch.was_lost(Some(7), 499));
        assert!(!watch.was_lost(Some(42), 501));
        assert!(watch.was_lost(Some(7), 501));

        let watch = DialogFocus {
            started_at: u32::MAX,
            ..watch
        };
        assert!(watch.was_lost(Some(7), 1));
    }
}
