//! The preview pane beside the result list: a thumbnail and the basic facts of the
//! highlighted media file, read from Windows in the background and kept for a while.

use super::*;
use crate::media_preview::{self, MediaFacts, MediaPreviewLoader, PreviewRequest};
use std::{collections::VecDeque, time::SystemTime};

/// Files whose thumbnails stay in memory, so going back to a row shows it at once.
const CACHE_SIZE: usize = 50;
/// The pane's thumbnail box in logical pixels: the pane's 212px, less its 1px divider and
/// 12px padding each side, by 106px. Must match the pane in ui/spotlight.slint.
const THUMBNAIL_BOX: (f32, f32) = (187.0, 106.0);

/// The thumbnail box in physical pixels at `scale`, so Windows makes thumbnails no
/// larger than the screen shows them.
pub(super) fn thumbnail_size(scale: f32) -> (u32, u32) {
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let pixels = |logical: f32| (logical * scale).ceil() as u32;
    (pixels(THUMBNAIL_BOX.0), pixels(THUMBNAIL_BOX.1))
}

struct CachedFacts {
    key: String,
    /// What the file's modified time was when it was read.
    modified: Option<SystemTime>,
    size: (u32, u32),
    thumbnail: Option<slint::Image>,
    dimensions: Option<(u32, u32)>,
    duration: Option<Duration>,
}

#[derive(Default)]
pub(super) struct MediaPane {
    loader: Option<MediaPreviewLoader>,
    /// Most recently read first.
    cache: VecDeque<CachedFacts>,
    /// The file the pane shows, by path key.
    shown: Option<String>,
    /// The last file asked of the loader whose facts haven't arrived.
    requested: Option<String>,
}

impl MediaPane {
    /// What was last read about file `key`, and whether it still holds: the file is
    /// unchanged since and the thumbnail was made for this size.
    fn cached(&self, key: &str, modified: Option<SystemTime>, size: (u32, u32)) -> Option<(&CachedFacts, bool)> {
        let facts = self.cache.iter().find(|facts| facts.key == key)?;
        Some((facts, facts.modified == modified && facts.size == size))
    }

    fn request(&mut self, key: &str, path: &Path, size: (u32, u32)) {
        if self.requested.as_deref() == Some(key) {
            return;
        }
        if self.loader.is_none() {
            self.loader = MediaPreviewLoader::start(|facts| {
                post(&MEDIA_FACTS_DISPATCH, facts);
            });
        }
        let request = PreviewRequest { key: key.to_string(), path: path.to_path_buf(), size };
        if self.loader.as_ref().is_some_and(|loader| loader.request(request)) {
            self.requested = Some(key.to_string());
        } else {
            self.loader = None;
        }
    }

    #[cfg(test)]
    pub(super) fn requested(&self) -> Option<&str> {
        self.requested.as_deref()
    }

    fn store(&mut self, facts: MediaFacts) {
        if self.requested.as_deref() == Some(facts.key.as_str()) {
            self.requested = None;
        }
        self.cache.retain(|cached| cached.key != facts.key);
        self.cache.push_front(CachedFacts {
            thumbnail: facts.thumbnail.map(slint::Image::from_rgb8),
            key: facts.key,
            modified: facts.modified,
            size: facts.size,
            dimensions: facts.dimensions,
            duration: facts.duration,
        });
        self.cache.truncate(CACHE_SIZE);
    }
}

/// What the pane shows for one result, before Windows has been asked about the file.
struct PaneSubject<'a> {
    result: &'a SearchResult,
    name: &'a str,
    media_type: Option<MediaType>,
    path: Option<&'a str>,
    note: &'static str,
}

fn pane_subject<'a>(result: &'a SearchResult, resolved: &SearchResult) -> Option<PaneSubject<'a>> {
    match result {
        SearchResult::ProjectItem(item) if item.kind == ProjectItemKind::Media => Some(PaneSubject {
            result,
            name: &item.name,
            media_type: item.media_type,
            path: item.media_path.as_deref().filter(|path| !path.is_empty()),
            note: if item.offline { "Media offline" } else { "" },
        }),
        SearchResult::LibraryFile(file) => Some(PaneSubject {
            result,
            name: &file.name,
            media_type: Some(file.media_type),
            path: file.path.to_str(),
            note: if matches!(resolved, SearchResult::LibraryFile(_)) { "Not in the project yet" } else { "" },
        }),
        _ => None,
    }
}

fn media_label(media_type: Option<MediaType>) -> &'static str {
    match media_type {
        Some(MediaType::Video) => "Video",
        Some(MediaType::Audio) => "Audio",
        Some(MediaType::Image) => "Image",
        Some(MediaType::Unknown) | None => "Media",
    }
}

/// Short clips show tenths of a second, as the audio preview does.
fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 10 {
        format!("0:{:02}.{}", seconds, duration.subsec_millis() / 100)
    } else if seconds >= 3600 {
        format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}

/// "Video · 1920 × 1080 · 0:04": the kind, then whatever Windows knows. Images have no
/// duration, audio no frame size.
pub(super) fn facts_line(media_type: Option<MediaType>, dimensions: Option<(u32, u32)>, duration: Option<Duration>) -> String {
    let mut parts = vec![media_label(media_type).to_string()];
    if media_type != Some(MediaType::Audio) {
        if let Some((width, height)) = dimensions {
            parts.push(format!("{width} × {height}"));
        }
    }
    if media_type != Some(MediaType::Image) {
        if let Some(duration) = duration.filter(|duration| !duration.is_zero()) {
            parts.push(format_duration(duration));
        }
    }
    parts.join(" · ")
}

/// Shows the pane for the highlighted row when it is a media file, and asks Windows
/// about the file when nothing current is cached. Hides it for every other row.
pub(super) fn sync_media_pane(ui: &SpotlightWindow, state: &mut AppState) {
    show_media_pane(ui, state, true);
}

/// `may_request` is false when answering the loader, so a file that keeps changing,
/// such as one being rendered, isn't read again and again while it stays highlighted.
fn show_media_pane(ui: &SpotlightWindow, state: &mut AppState, may_request: bool) {
    let selected = (state.is_visible
        && state.settings.show_preview_pane
        && !ui.get_settings_open()
        && state.marker_picker.is_none()
        && state.marker_naming.is_none())
    .then(|| state.matched_items.get(state.selected_abs_index))
    .flatten();
    let resolved = selected.map(|result| state.catalog.resolve(result));
    let Some(subject) = selected.zip(resolved.as_ref()).and_then(|(result, resolved)| pane_subject(result, resolved)) else {
        state.media_pane.shown = None;
        if ui.get_media_pane().visible {
            ui.set_media_pane(MediaPaneUI::default());
        }
        return;
    };

    let key = subject.path.map(library::path_key).filter(|key| !key.is_empty());
    let size = thumbnail_size(ui.window().scale_factor());
    let modified = subject.path.and_then(|path| media_preview::modified_time(Path::new(path)));
    // A file changed since it was read shows what was read until the new read arrives.
    let lookup = key.as_deref().and_then(|key| state.media_pane.cached(key, modified, size));
    let current = lookup.is_some_and(|(_, current)| current);
    let cached = lookup.map(|(facts, _)| facts);
    let icon = kind_icon(subject.result);
    let color = |(r, g, b): (u8, u8, u8)| Color::from_rgb_u8(r, g, b);
    let thumbnail = cached.and_then(|facts| facts.thumbnail.clone());
    let pane = MediaPaneUI {
        visible: true,
        name: subject.name.into(),
        facts: facts_line(
            subject.media_type,
            cached.and_then(|facts| facts.dimensions),
            cached.and_then(|facts| facts.duration),
        )
        .into(),
        path: subject.path.unwrap_or("").into(),
        note: subject.note.into(),
        icon: icon.name.into(),
        tint: color(icon.tint),
        ink: color(icon.ink),
        has_thumbnail: thumbnail.is_some(),
        thumbnail: thumbnail.unwrap_or_default(),
    };
    ui.set_media_pane(pane);
    state.media_pane.shown = key.clone();
    if let (Some(key), Some(path), false, true) = (key, subject.path, current, may_request) {
        state.media_pane.request(&key, Path::new(path), size);
    }
}

/// Keeps what Windows found about a file, and shows it if the pane still shows that file.
pub(super) fn receive_media_facts(ui: &SpotlightWindow, state: &mut AppState, facts: MediaFacts) {
    let shown = state.media_pane.shown.as_deref() == Some(facts.key.as_str());
    state.media_pane.store(facts);
    if shown {
        show_media_pane(ui, state, false);
    }
}
