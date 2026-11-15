mod playback;

use crate::{
    bridge::{AudioPreviewResult, BridgeClients, MediaCatalog},
    catalog::{ProjectItemEntry, ProjectItemKind},
    SpotlightWindow,
};
use playback::{Command, Worker};
use slint::{Timer, TimerMode};
use std::time::{Duration, Instant};

const AUTOPLAY_DELAY: Duration = Duration::from_millis(250);
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(PartialEq, Eq)]
struct Selection {
    peer_id: usize,
    project_id: String,
    item_id: String,
    offline: bool,
    audio_name: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResolveIntent {
    Identify,
    Autoplay,
    Manual,
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Debouncing {
        deadline: Instant,
        intent: ResolveIntent,
    },
    Resolving {
        request_id: u64,
        deadline: Instant,
        intent: ResolveIntent,
    },
    Loading,
    Ready,
}

#[derive(Default)]
pub struct AudioPreview {
    selection: Option<Selection>,
    state: State,
    timer: Timer,
    worker: Option<Worker>,
    generation: u64,
    pending_commands: usize,
}

impl AudioPreview {
    pub fn set_poll_callback(&self, callback: impl Fn() + 'static) {
        self.timer
            .start(TimerMode::Repeated, Duration::from_millis(50), callback);
        self.timer.stop();
    }

    pub fn select(
        &mut self,
        ui: &SpotlightWindow,
        catalog: Option<&MediaCatalog>,
        item: Option<&ProjectItemEntry>,
        auto_preview: bool,
    ) {
        let selection = catalog.zip(item).and_then(|(catalog, item)| {
            (item.kind == ProjectItemKind::Media).then_some(())?;
            Some(Selection {
                peer_id: catalog.peer_id,
                project_id: catalog.project_id.clone()?,
                item_id: item.item_id.clone(),
                offline: item.offline,
                // Match the bridge's fallback for standalone audio filenames.
                audio_name: std::path::Path::new(&item.name)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        matches!(
                            extension.to_ascii_lowercase().as_str(),
                            "wav"
                                | "wave"
                                | "bwf"
                                | "aif"
                                | "aiff"
                                | "aifc"
                                | "flac"
                                | "mp3"
                                | "aac"
                                | "m4a"
                                | "alac"
                        )
                    }),
            })
        });
        if self.selection == selection {
            return;
        }
        self.cancel(ui);
        self.selection = selection;
        let Some(item) = &self.selection else { return };
        ui.set_preview_active(item.audio_name);
        ui.set_preview_can_toggle(item.audio_name);
        if !item.offline && (auto_preview || !item.audio_name) {
            let intent = if auto_preview {
                ResolveIntent::Autoplay
            } else {
                ResolveIntent::Identify
            };
            self.state = State::Debouncing {
                deadline: Instant::now() + AUTOPLAY_DELAY,
                intent,
            };
            self.timer.restart();
        }
    }

    pub fn defer_autoplay(&mut self) {
        if let State::Debouncing { deadline, .. } = &mut self.state {
            *deadline = Instant::now() + AUTOPLAY_DELAY;
        }
    }

    pub fn cancel(&mut self, ui: &SpotlightWindow) {
        if self.selection.take().is_none() {
            return;
        }
        self.invalidate_worker();
        self.state = State::Idle;
        self.timer.stop();
        ui.set_preview_active(false);
        ui.set_preview_message("".into());
        ui.set_preview_time("".into());
        ui.set_preview_progress(0.0);
        ui.set_preview_can_seek(false);
        ui.set_preview_can_toggle(false);
        ui.set_preview_playing(false);
    }

    fn invalidate_worker(&mut self) {
        self.generation += 1;
        self.pending_commands = 0;
        if let Some(worker) = &self.worker {
            worker.cancel(self.generation);
        }
    }

    pub fn toggle(
        &mut self,
        ui: &SpotlightWindow,
        bridge: &BridgeClients,
        catalog: Option<&MediaCatalog>,
    ) {
        match &mut self.state {
            State::Ready => self.send(ui, Command::Toggle),
            State::Resolving { intent, .. } => *intent = ResolveIntent::Manual,
            State::Loading => {}
            State::Idle | State::Debouncing { .. } => {
                self.resolve(ui, bridge, catalog, ResolveIntent::Manual);
            }
        }
    }

    pub fn seek(&mut self, ui: &SpotlightWindow, fraction: f32) {
        if matches!(self.state, State::Ready) && ui.get_preview_can_seek() && fraction.is_finite() {
            self.send(ui, Command::Seek(fraction.clamp(0.0, 1.0)));
        }
    }

    fn resolve(
        &mut self,
        ui: &SpotlightWindow,
        bridge: &BridgeClients,
        catalog: Option<&MediaCatalog>,
        intent: ResolveIntent,
    ) {
        let request = self
            .selection
            .as_ref()
            .zip(catalog)
            .ok_or("Select an audio clip to preview.")
            .and_then(|(selection, catalog)| {
                bridge.request_audio_preview(catalog, &selection.item_id)
            });
        match request {
            Ok(request_id) => {
                self.state = State::Resolving {
                    request_id,
                    deadline: Instant::now() + RESOLVE_TIMEOUT,
                    intent,
                };
                if !self.timer.running() {
                    self.timer.restart();
                }
            }
            Err(message) => self.resolve_failed(ui, intent, message),
        }
    }

    fn resolve_failed(&mut self, ui: &SpotlightWindow, intent: ResolveIntent, message: &str) {
        self.state = State::Idle;
        self.timer.stop();
        if intent == ResolveIntent::Manual {
            if !ui.get_preview_active() {
                ui.set_status_message(message.into());
            } else {
                ui.set_preview_message(message.into());
            }
        }
    }

    pub fn resolved(&mut self, ui: &SpotlightWindow, result: AudioPreviewResult) {
        let State::Resolving {
            request_id, intent, ..
        } = self.state
        else {
            return;
        };
        let Some(selection) = &self.selection else {
            return;
        };
        if request_id != result.request_id || selection.peer_id != result.peer_id {
            return;
        }
        let Some(source) = result.source else {
            let message = if result.message.is_empty() {
                "Select an audio-only clip to preview."
            } else {
                &result.message
            };
            self.resolve_failed(ui, intent, message);
            return;
        };
        ui.set_preview_active(true);
        if intent == ResolveIntent::Identify {
            ui.set_preview_can_toggle(true);
            self.state = State::Idle;
            self.timer.stop();
            return;
        }
        ui.set_preview_message("Loading audio…".into());
        ui.set_preview_can_toggle(false);
        self.state = State::Loading;
        if self.worker.is_none() {
            self.worker = Worker::spawn(self.generation).ok();
        }
        self.send(ui, Command::Load(source));
    }

    fn send(&mut self, ui: &SpotlightWindow, command: Command) {
        if !self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.send(self.generation, command))
        {
            self.worker = None;
            self.playback_failed(ui, "Could not start the audio player.");
        } else {
            self.pending_commands += 1;
            if !self.timer.running() {
                self.timer.restart();
            }
        }
    }

    fn playback_failed(&mut self, ui: &SpotlightWindow, message: &str) {
        self.invalidate_worker();
        self.state = State::Idle;
        self.timer.stop();
        ui.set_preview_message(message.into());
        ui.set_preview_playing(false);
        ui.set_preview_can_seek(false);
        ui.set_preview_can_toggle(true);
    }

    pub fn poll(
        &mut self,
        ui: &SpotlightWindow,
        bridge: &BridgeClients,
        catalog: Option<&MediaCatalog>,
    ) {
        match self.state {
            State::Debouncing { deadline, intent } if Instant::now() >= deadline => {
                self.resolve(ui, bridge, catalog, intent);
            }
            State::Resolving {
                deadline, intent, ..
            } if Instant::now() >= deadline => {
                self.resolve_failed(
                    ui,
                    intent,
                    "Premiere did not respond to the preview request.",
                );
            }
            _ => {}
        }
        while let Some(event) = self.worker.as_ref().and_then(Worker::next_update) {
            if event.generation != self.generation
                || !matches!(self.state, State::Loading | State::Ready)
            {
                continue;
            }
            if event.command_completed {
                self.pending_commands = self.pending_commands.saturating_sub(1);
            }
            match event.result {
                Ok(snapshot) => {
                    self.state = State::Ready;
                    ui.set_preview_message("".into());
                    ui.set_preview_playing(snapshot.playing);
                    ui.set_preview_can_toggle(true);
                    ui.set_preview_can_seek(
                        snapshot
                            .duration
                            .is_some_and(|duration| !duration.is_zero()),
                    );
                    let show_tenths = snapshot
                        .duration
                        .is_some_and(|duration| duration.as_secs() < 10);
                    let elapsed = format_time(snapshot.position, show_tenths);
                    let time = snapshot.duration.map_or_else(
                        || format!("{elapsed} / —"),
                        |duration| format!("{elapsed} / {}", format_time(duration, show_tenths)),
                    );
                    ui.set_preview_time(time.into());
                    ui.set_preview_progress(
                        snapshot
                            .duration
                            .filter(|duration| !duration.is_zero())
                            .map_or(0.0, |duration| {
                                (snapshot.position.as_secs_f64() / duration.as_secs_f64()) as f32
                            })
                            .clamp(0.0, 1.0),
                    );
                    if !snapshot.playing && self.pending_commands == 0 {
                        self.timer.stop();
                    }
                }
                Err(message) => self.playback_failed(ui, message),
            }
        }
    }
}

fn format_time(duration: Duration, show_tenths: bool) -> String {
    let seconds = duration.as_secs();
    let hours = seconds / 3600;
    if show_tenths {
        format!(
            "{}:{:02}.{}",
            seconds / 60,
            seconds % 60,
            duration.subsec_millis() / 100,
        )
    } else if hours > 0 {
        format!("{hours}:{:02}:{:02}", seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
