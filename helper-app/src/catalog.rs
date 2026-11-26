mod search;

use crate::{
    actions::Action,
    library::{self, LibraryEntry},
    protocol::SequenceMarker,
};
use nucleo_matcher::{Config, Matcher};
use search::{Query, SearchKey};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

/// Bin name the plugin reports for items at the top of the Project panel.
/// Must match `PROJECT_ROOT_BIN` in `uxp-plugin/index.js`.
pub const PROJECT_ROOT_BIN: &str = "Project root";
/// Joins nested bin and preset folder names. Must match `BIN_PATH_SEPARATOR`
/// in `uxp-plugin/index.js`.
pub const BIN_PATH_SEPARATOR: &str = " / ";

/// A search scope: one of the tabs below the search field. The ids are the
/// values the Slint `active-catalog` property uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Catalog {
    All,
    Effects,
    Project,
    Library,
    Actions,
}

impl Catalog {
    pub const ALL: [Self; 5] = [Self::All, Self::Effects, Self::Project, Self::Library, Self::Actions];

    pub fn id(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Effects => "effects",
            Self::Project => "project",
            Self::Library => "library",
            Self::Actions => "actions",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|catalog| catalog.id() == id)
    }

    fn includes_effects(self) -> bool {
        matches!(self, Self::All | Self::Effects)
    }

    fn includes_project(self) -> bool {
        matches!(self, Self::All | Self::Project)
    }

    fn includes_library(self) -> bool {
        matches!(self, Self::All | Self::Library)
    }

    fn includes_actions(self) -> bool {
        matches!(self, Self::All | Self::Actions)
    }
}

/// The kind of an effect entry. Serialized with the same camelCase names the
/// UXP plugin uses for `effectType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EffectType {
    VideoEffect,
    AudioEffect,
    VideoTransition,
    AudioTransition,
    Preset,
    /// A type a newer plugin may send. It is kept out of search results.
    #[serde(other)]
    Unknown,
}

impl EffectType {
    pub fn id(self) -> &'static str {
        match self {
            Self::VideoEffect => "videoEffect",
            Self::AudioEffect => "audioEffect",
            Self::VideoTransition => "videoTransition",
            Self::AudioTransition => "audioTransition",
            Self::Preset => "preset",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_transition(self) -> bool {
        matches!(self, Self::VideoTransition | Self::AudioTransition)
    }

    /// The kind's full name, which search matches as context.
    fn label(self) -> &'static str {
        match self {
            Self::VideoEffect => "Video Effect",
            Self::AudioEffect => "Audio Effect",
            Self::VideoTransition => "Video Transition",
            Self::AudioTransition => "Audio Transition",
            Self::Preset => "Preset",
            Self::Unknown => "Effect",
        }
    }
}

impl From<&str> for EffectType {
    fn from(id: &str) -> Self {
        [
            Self::VideoEffect,
            Self::AudioEffect,
            Self::VideoTransition,
            Self::AudioTransition,
            Self::Preset,
        ]
        .into_iter()
        .find(|kind| kind.id() == id)
        .unwrap_or(Self::Unknown)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectEntry {
    pub name: String,
    pub match_name: String,
    pub effect_type: EffectType,
    #[serde(default)]
    pub folder: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItemEntry {
    pub item_id: String,
    pub name: String,
    pub bin_path: String,
    pub kind: ProjectItemKind,
    #[serde(default)]
    pub offline: bool,
    /// What a media clip holds, for its icon. Null from older plugins and for items
    /// without a file extension.
    #[serde(default)]
    pub media_type: Option<MediaType>,
    /// The clip's file on disk. Null from older plugins, for sequences and bins, and
    /// when Premiere can't report it.
    #[serde(default)]
    pub media_path: Option<String>,
}

/// The kind of file behind a media clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MediaType {
    Video,
    Image,
    Audio,
    /// A type a newer plugin may send.
    #[serde(other)]
    Unknown,
}

impl ProjectItemEntry {
    pub fn placement_error(&self) -> Option<&'static str> {
        match self.kind {
            ProjectItemKind::Bin => {
                Some("Bins can't be placed on the timeline. Bins are search only.")
            }
            ProjectItemKind::ActiveSequence => Some("A sequence can't nest into itself."),
            ProjectItemKind::Other => Some("This project item can't be placed on the timeline."),
            _ if self.offline => Some("Relink the offline media before placing it."),
            _ => None,
        }
    }

    pub fn source_monitor_error(&self) -> Option<&'static str> {
        match self.kind {
            ProjectItemKind::Bin => Some("Bins can't open in the Source Monitor."),
            ProjectItemKind::Other => Some("This project item can't open in the Source Monitor."),
            _ => None,
        }
    }

    /// Returns the `bin_path` shared by direct children inside this bin, if it is a bin.
    pub fn child_bin_path(&self) -> Option<String> {
        if self.kind != ProjectItemKind::Bin {
            return None;
        }
        Some(if self.bin_path == PROJECT_ROOT_BIN {
            self.name.clone()
        } else {
            format!("{}{BIN_PATH_SEPARATOR}{}", self.bin_path, self.name)
        })
    }
}

/// The bin path one level above `bin_path`, or `None` at the top level.
pub fn parent_bin_path(bin_path: &str) -> Option<&str> {
    bin_path.rsplit_once(BIN_PATH_SEPARATOR).map(|(parent, _)| parent)
}

fn in_bin_scope(item_bin_path: &str, scope: &str) -> bool {
    item_bin_path == scope
        || item_bin_path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with(BIN_PATH_SEPARATOR))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectItemKind {
    Media,
    Sequence,
    ActiveSequence,
    Bin,
    Other,
}

impl ProjectItemKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Media => "Media",
            Self::Sequence | Self::ActiveSequence => "Sequence",
            Self::Bin => "Bin",
            Self::Other => "Project Item",
        }
    }
}

#[derive(Clone)]
pub enum SearchResult {
    Effect(Arc<EffectEntry>),
    ProjectItem(Arc<ProjectItemEntry>),
    Action(Action),
    Marker(Arc<SequenceMarker>),
    /// A file in a tracked Library folder.
    LibraryFile(Arc<LibraryEntry>),
}

struct IndexedEffect {
    entry: Arc<EffectEntry>,
    name_lower: String,
    key: SearchKey,
    supported: bool,
}

impl IndexedEffect {
    fn new(entry: EffectEntry) -> Self {
        let name_lower = entry.name.to_lowercase();
        // The type label lets "transition" or "audio" find every entry of that kind.
        let key = SearchKey::new(
            &entry.name,
            &format!("{} {}", entry.folder, entry.effect_type.label()),
        );
        let supported = entry.is_supported();
        Self {
            entry: Arc::new(entry),
            name_lower,
            key,
            supported,
        }
    }
}

struct IndexedProjectItem {
    entry: Arc<ProjectItemEntry>,
    name_lower: String,
    key: SearchKey,
}

impl IndexedProjectItem {
    fn new(entry: ProjectItemEntry) -> Self {
        let name_lower = entry.name.to_lowercase();
        let key = SearchKey::new(
            &entry.name,
            &format!("{} {}", entry.bin_path, entry.kind.label()),
        );
        Self {
            entry: Arc::new(entry),
            name_lower,
            key,
        }
    }
}

struct IndexedLibraryFile {
    entry: Arc<LibraryEntry>,
    name_lower: String,
    key: SearchKey,
}

impl IndexedLibraryFile {
    fn new(entry: LibraryEntry) -> Self {
        let kind = match entry.media_type {
            MediaType::Audio => "Audio",
            MediaType::Image => "Image",
            MediaType::Video | MediaType::Unknown => "Video",
        };
        let key = SearchKey::new(&entry.name, &format!("{} Library {kind}", entry.folder));
        Self {
            name_lower: entry.name.to_lowercase(),
            entry: Arc::new(entry),
            key,
        }
    }
}

struct IndexedAction {
    action: Action,
    name_lower: String,
    key: SearchKey,
}

impl IndexedAction {
    fn new(action: Action) -> Self {
        Self {
            action,
            name_lower: action.name().to_lowercase(),
            key: SearchKey::new(action.name(), action.description()),
        }
    }
}

#[derive(Clone, Copy)]
enum CatalogEntry<'a> {
    Effect(&'a IndexedEffect),
    ProjectItem(&'a IndexedProjectItem),
    Action(&'a IndexedAction),
    LibraryFile(&'a IndexedLibraryFile),
}

/// Ranking bonus for an effect applied `count` times this session. It grows
/// with each doubling and is capped, so habit reorders similar matches without
/// burying an exact match of a less-used effect.
fn usage_bonus(count: u32) -> u32 {
    (40 * (count.saturating_add(1)).ilog2()).min(160)
}

impl SearchResult {
    pub fn same_item(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Effect(a), Self::Effect(b)) => {
                a.effect_type == b.effect_type && a.match_name == b.match_name
            }
            (Self::ProjectItem(a), Self::ProjectItem(b)) => a.item_id == b.item_id,
            (Self::Action(a), Self::Action(b)) => a == b,
            (Self::Marker(a), Self::Marker(b)) => a == b,
            (Self::LibraryFile(a), Self::LibraryFile(b)) => a.key == b.key,
            _ => false,
        }
    }
}

impl CatalogEntry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Effect(item) => &item.entry.name,
            Self::ProjectItem(item) => &item.entry.name,
            Self::Action(item) => item.action.name(),
            Self::LibraryFile(item) => &item.entry.name,
        }
    }

    fn name_lower(&self) -> &str {
        match self {
            Self::Effect(item) => &item.name_lower,
            Self::ProjectItem(item) => &item.name_lower,
            Self::Action(item) => &item.name_lower,
            Self::LibraryFile(item) => &item.name_lower,
        }
    }

    fn folder(&self) -> &str {
        match self {
            Self::Effect(item) => &item.entry.folder,
            Self::ProjectItem(item) => &item.entry.bin_path,
            Self::Action(_) => "",
            Self::LibraryFile(item) => &item.entry.folder,
        }
    }

    fn key(&self) -> &SearchKey {
        match self {
            Self::Effect(item) => &item.key,
            Self::ProjectItem(item) => &item.key,
            Self::Action(item) => &item.key,
            Self::LibraryFile(item) => &item.key,
        }
    }

    fn to_result(self) -> SearchResult {
        match self {
            Self::Effect(item) => SearchResult::Effect(Arc::clone(&item.entry)),
            Self::ProjectItem(item) => SearchResult::ProjectItem(Arc::clone(&item.entry)),
            Self::Action(item) => SearchResult::Action(item.action),
            Self::LibraryFile(item) => SearchResult::LibraryFile(Arc::clone(&item.entry)),
        }
    }

    /// Breaks ties between equal scores: Library files come after everything else.
    fn tie_group(&self) -> u8 {
        u8::from(matches!(self, Self::LibraryFile(_)))
    }
}

impl EffectEntry {
    pub fn is_preset(&self) -> bool {
        self.effect_type == EffectType::Preset
    }

    fn is_supported(&self) -> bool {
        matches!(
            self.effect_type,
            EffectType::VideoEffect | EffectType::AudioEffect | EffectType::VideoTransition | EffectType::Preset
        )
    }
}

pub struct CatalogStore {
    items: Vec<IndexedEffect>,
    presets: Vec<IndexedEffect>,
    project_items: Vec<IndexedProjectItem>,
    /// Project media by the [`library::path_key`] of its file.
    project_paths: HashMap<String, Arc<ProjectItemEntry>>,
    library: Vec<IndexedLibraryFile>,
    /// Each Library file's position in `library`, by its [`library::path_key`].
    /// Rebuilt with `library`, so the two always agree.
    library_by_key: HashMap<String, usize>,
    /// How often each effect was applied this session, by type and match name.
    usage_counts: HashMap<EffectType, HashMap<String, u32>>,
    actions: Vec<IndexedAction>,
    matcher: Matcher,
}

impl CatalogStore {
    pub fn new_with_defaults() -> Self {
        let items = default_premiere_effects()
            .into_iter()
            .map(IndexedEffect::new)
            .collect();
        Self {
            items,
            presets: Vec::new(),
            project_items: Vec::new(),
            project_paths: HashMap::new(),
            library: Vec::new(),
            library_by_key: HashMap::new(),
            usage_counts: HashMap::new(),
            actions: Action::ALL.into_iter().map(IndexedAction::new).collect(),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn set_catalog(&mut self, new_items: Vec<EffectEntry>) {
        let mut indexed: Vec<IndexedEffect> = new_items
            .into_iter()
            .filter(|entry| !entry.is_preset())
            .map(IndexedEffect::new)
            .collect();
        if indexed.is_empty() {
            return;
        }
        indexed.sort_by(|a, b| a.name_lower.cmp(&b.name_lower));
        indexed.dedup_by(|a, b| {
            a.entry.effect_type == b.entry.effect_type
                && a.entry.match_name == b.entry.match_name
                && a.entry.name == b.entry.name
        });
        self.items = indexed;
    }

    pub fn set_presets(&mut self, presets: Vec<EffectEntry>) {
        self.presets = presets.into_iter().map(IndexedEffect::new).collect();
    }

    pub fn record_usage(&mut self, entry: &EffectEntry) {
        let count = self
            .usage_counts
            .entry(entry.effect_type)
            .or_default()
            .entry(entry.match_name.clone())
            .or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn set_project_items(&mut self, items: Vec<ProjectItemEntry>) {
        self.project_items = items
            .into_iter()
            .map(IndexedProjectItem::new)
            .collect();
        self.project_paths.clear();
        for item in &self.project_items {
            let entry = &item.entry;
            let Some(path) = entry.media_path.as_deref().filter(|_| entry.kind == ProjectItemKind::Media) else {
                continue;
            };
            // A file imported twice maps to its first clip in the Project panel.
            self.project_paths
                .entry(library::path_key(path))
                .or_insert_with(|| Arc::clone(entry));
        }
    }

    pub fn set_library_files(&mut self, entries: Vec<LibraryEntry>) {
        self.library = entries.into_iter().map(IndexedLibraryFile::new).collect();
        self.library_by_key = self
            .library
            .iter()
            .enumerate()
            .map(|(index, file)| (file.entry.key.clone(), index))
            .collect();
    }

    /// The project clip already made from `file`, if the project holds it.
    pub fn project_item_for(&self, file: &LibraryEntry) -> Option<Arc<ProjectItemEntry>> {
        self.project_paths.get(&file.key).cloned()
    }

    /// The indexed Library file with this [`library::path_key`].
    pub fn library_file(&self, key: &str) -> Option<Arc<LibraryEntry>> {
        let index = *self.library_by_key.get(key)?;
        self.library.get(index).map(|file| Arc::clone(&file.entry))
    }

    /// What `result` stands for: a Library file the project already holds is that
    /// project clip, so Enter places it instead of importing it again.
    pub fn resolve(&self, result: &SearchResult) -> SearchResult {
        match result {
            SearchResult::LibraryFile(file) => self
                .project_item_for(file)
                .map_or_else(|| result.clone(), SearchResult::ProjectItem),
            _ => result.clone(),
        }
    }

    /// Effects and presets, project items, and Library files.
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.items.len() + self.presets.len(), self.project_items.len(), self.library.len())
    }

    pub fn contains_bin_path(&self, bin_path: &str) -> bool {
        self.project_items
            .iter()
            .any(|item| item.entry.child_bin_path().as_deref() == Some(bin_path))
    }

    #[cfg(test)]
    pub fn search(&mut self, query: &str, catalog: &str) -> Vec<SearchResult> {
        let catalog = Catalog::from_id(catalog).expect("a catalog id");
        self.search_in_bin(query, catalog, None)
    }

    /// Markers matching `query`, best match first. Equal matches, and every
    /// marker for a blank query, keep the order of `markers`.
    pub fn search_markers(&mut self, query: &str, markers: &[Arc<SequenceMarker>]) -> Vec<SearchResult> {
        let Some(query) = Query::new(query) else {
            return markers.iter().cloned().map(SearchResult::Marker).collect();
        };
        let mut scored: Vec<_> = markers
            .iter()
            .filter_map(|marker| {
                let context = format!("{} {} {}", marker.sequence_name, marker.comments, marker.position());
                let key = SearchKey::new(marker.title(), &context);
                Some((query.score(&key, &mut self.matcher)?, marker))
            })
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored
            .into_iter()
            .map(|(_, marker)| SearchResult::Marker(Arc::clone(marker)))
            .collect()
    }

    pub fn search_in_bin(
        &mut self,
        query: &str,
        catalog: Catalog,
        bin_scope: Option<&str>,
    ) -> Vec<SearchResult> {
        let trimmed = query.trim();
        let include_effects = catalog.includes_effects() && bin_scope.is_none();
        let effects = self
            .items
            .iter()
            .chain(&self.presets)
            .filter(move |item| include_effects && item.supported)
            .map(CatalogEntry::Effect);
        let include_project = catalog.includes_project();
        let project_items = self
            .project_items
            .iter()
            .filter(move |item| {
                include_project
                    && bin_scope.is_none_or(|scope| in_bin_scope(&item.entry.bin_path, scope))
            })
            .map(CatalogEntry::ProjectItem);
        let include_actions = catalog.includes_actions() && bin_scope.is_none();
        let actions = self
            .actions
            .iter()
            .filter(move |_| include_actions)
            .map(CatalogEntry::Action);
        let include_library = catalog.includes_library() && bin_scope.is_none();
        // All shows a file the project already holds once, as its project clip.
        let hide_imported = catalog == Catalog::All;
        let project_paths = &self.project_paths;
        let library = self
            .library
            .iter()
            .filter(move |file| {
                include_library && !(hide_imported && project_paths.contains_key(&file.entry.key))
            })
            .map(CatalogEntry::LibraryFile);
        let entries = effects.chain(project_items).chain(actions).chain(library);
        let usage = |item: &CatalogEntry| match item {
            CatalogEntry::Effect(indexed) => self
                .usage_counts
                .get(&indexed.entry.effect_type)
                .and_then(|counts| counts.get(&indexed.entry.match_name))
                .copied()
                .unwrap_or(0),
            CatalogEntry::ProjectItem(_) | CatalogEntry::Action(_) | CatalogEntry::LibraryFile(_) => 0,
        };

        if trimmed.is_empty() {
            let mut filtered: Vec<_> = entries.collect();

            filtered.sort_by(|a, b| {
                let scope_order = |entry: &CatalogEntry| match (bin_scope, entry) {
                    (Some(scope), CatalogEntry::ProjectItem(item)) => (
                        item.entry.bin_path != scope,
                        item.entry.kind != ProjectItemKind::Bin,
                    ),
                    _ => (false, false),
                };
                scope_order(a)
                    .cmp(&scope_order(b))
                    .then_with(|| a.tie_group().cmp(&b.tie_group()))
                    .then_with(|| usage(b).cmp(&usage(a)))
                    .then_with(|| a.name_lower().cmp(b.name_lower()))
                    .then_with(|| a.folder().cmp(b.folder()))
            });
            return filtered.into_iter().map(CatalogEntry::to_result).collect();
        }

        let Some(query) = Query::new(trimmed) else {
            return Vec::new();
        };
        let mut scored: Vec<_> = entries
            .filter_map(|item| {
                let score = query.score(item.key(), &mut self.matcher)?;
                Some((score + usage_bonus(usage(&item)), item))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.tie_group().cmp(&b.1.tie_group()))
                .then_with(|| a.1.name().len().cmp(&b.1.name().len()))
                .then_with(|| a.1.name_lower().cmp(b.1.name_lower()))
                .then_with(|| a.1.folder().cmp(b.1.folder()))
        });

        scored
            .into_iter()
            .map(|(_, item)| item.to_result())
            .collect()
    }
}

fn default_premiere_effects() -> Vec<EffectEntry> {
    let raw = [
        // Video Effects
        ("Gaussian Blur", "AE.ADBE Gaussian Blur 2", "videoEffect", "Blur & Sharpen"),
        ("Transform", "AE.ADBE Geometry2", "videoEffect", "Distort"),
        ("Warp Stabilizer", "AE.ADBE SubspaceStabilizer", "videoEffect", "Distort"),
        ("Lumetri Color", "AE.ADBE Lumetri", "videoEffect", "Color Correction"),
        ("Crop", "AE.ADBE Crop", "videoEffect", "Transform"),
        ("Drop Shadow", "AE.ADBE Drop Shadow", "videoEffect", "Perspective"),
        ("Ultra Key", "AE.ADBE Ultra Key", "videoEffect", "Keying"),
        ("Luma Key", "AE.ADBE Luma Key", "videoEffect", "Keying"),
        ("Track Matte Key", "AE.ADBE Track Matte Key", "videoEffect", "Keying"),
        ("Color Key", "AE.ADBE Color Key", "videoEffect", "Keying"),
        ("Directional Blur", "AE.ADBE Motion Blur", "videoEffect", "Blur & Sharpen"),
        ("Camera Blur", "AE.ADBE Camera Blur", "videoEffect", "Blur & Sharpen"),
        ("Sharpen", "AE.ADBE Sharpen", "videoEffect", "Blur & Sharpen"),
        ("Unsharp Mask", "AE.ADBE Unsharp Mask", "videoEffect", "Blur & Sharpen"),
        ("Corner Pin", "AE.ADBE Corner Pin", "videoEffect", "Distort"),
        ("Turbulent Displace", "AE.ADBE Turbulent Displace", "videoEffect", "Distort"),
        ("Mirror", "AE.ADBE Mirror", "videoEffect", "Distort"),
        ("Offset", "AE.ADBE Offset", "videoEffect", "Distort"),
        ("Wave Warp", "AE.ADBE Wave Warp", "videoEffect", "Distort"),
        ("Tint", "AE.ADBE Tint", "videoEffect", "Color Correction"),
        ("Brightness & Contrast", "AE.ADBE Brightness & Contrast 2", "videoEffect", "Color Correction"),
        ("Color Balance (HLS)", "AE.ADBE Color Balance (HLS)", "videoEffect", "Color Correction"),
        ("Horizontal Flip", "PR.ADBE Horizontal Flip", "videoEffect", "Transform"),
        ("Vertical Flip", "PR.ADBE Vertical Flip", "videoEffect", "Transform"),
        ("Mosaic", "AE.ADBE Mosaic", "videoEffect", "Stylize"),
        ("Posterize Time", "AE.ADBE Posterize Time", "videoEffect", "Time"),
        ("Echo", "AE.ADBE Echo", "videoEffect", "Time"),
        ("Noise", "AE.ADBE Noise2", "videoEffect", "Noise & Grain"),
        ("Ramp", "AE.ADBE Ramp", "videoEffect", "Generate"),
        ("Basic 3D", "AE.ADBE Basic 3D", "videoEffect", "Perspective"),
        // Audio Effects
        ("Parametric Equalizer", "ADBE Parametric EQ", "audioEffect", "Filter and EQ"),
        ("DeNoise", "ADBE AdaptiveNoiseReduction", "audioEffect", "Noise Reduction/Restoration"),
        ("DeReverb", "ADBE DeReverb", "audioEffect", "Noise Reduction/Restoration"),
        ("Dynamics", "ADBE Dynamics", "audioEffect", "Amplitude and Compression"),
        ("Multiband Compressor", "ADBE MultibandCompressor", "audioEffect", "Amplitude and Compression"),
        ("Single-band Compressor", "ADBE SingleBandCompressor", "audioEffect", "Amplitude and Compression"),
        ("Hard Limiter", "ADBE HardLimiter", "audioEffect", "Amplitude and Compression"),
        ("Studio Reverb", "ADBE StudioReverb", "audioEffect", "Reverb"),
        ("Convolution Reverb", "ADBE ConvolutionReverb", "audioEffect", "Reverb"),
        ("Mastering", "ADBE Mastering", "audioEffect", "Special"),
        ("Vocal Enhancer", "ADBE VocalEnhancer", "audioEffect", "Special"),
        ("Pitch Shifter", "ADBE PitchShifter", "audioEffect", "Time and Pitch"),
        ("Lowpass", "ADBE Lowpass", "audioEffect", "Filter and EQ"),
        ("Highpass", "ADBE Highpass", "audioEffect", "Filter and EQ"),
        // Video Transitions
        ("Cross Dissolve", "ADBE Cross Dissolve New", "videoTransition", "Dissolve"),
        ("Dip to Black", "ADBE Dip To Black", "videoTransition", "Dissolve"),
        ("Dip to White", "ADBE Dip To White", "videoTransition", "Dissolve"),
        ("Film Dissolve", "ADBE Film Dissolve", "videoTransition", "Dissolve"),
        ("Additive Dissolve", "ADBE Additive Dissolve", "videoTransition", "Dissolve"),
        ("Morph Cut", "ADBE MorphCut", "videoTransition", "Dissolve"),
        ("Push", "ADBE Push", "videoTransition", "Slide"),
        ("Whip", "ADBE Whip", "videoTransition", "Slide"),
        ("Wipe", "ADBE Wipe", "videoTransition", "Wipe"),
    ];

    raw.into_iter()
        .map(|(name, match_name, effect_type, folder)| EffectEntry {
            name: name.to_string(),
            match_name: match_name.to_string(),
            effect_type: effect_type.into(),
            folder: folder.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_combines_effects_and_project_items() {
        let mut store = CatalogStore::new_with_defaults();
        store.set_project_items(vec![
            ProjectItemEntry {
                item_id: "1".into(),
                name: "Gaussian Blur".into(),
                bin_path: "Archive / Stills".into(),
                kind: ProjectItemKind::Media,
                offline: false,
                media_type: None,
                media_path: None,
            },
            ProjectItemEntry {
                item_id: "2".into(),
                name: "Gaussian Blur".into(),
                bin_path: "Drafts".into(),
                kind: ProjectItemKind::Sequence,
                offline: false,
                media_type: None,
                media_path: None,
            },
        ]);
        let all = store.search("Gaussian Blur", "all");
        assert!(all
            .iter()
            .any(|item| matches!(item, SearchResult::Effect(_))));
        assert_eq!(
            all.iter()
                .filter(|item| matches!(item, SearchResult::ProjectItem(_)))
                .count(),
            2
        );
        assert!(matches!(store.search("Archive", "project").as_slice(),
            [SearchResult::ProjectItem(item)] if item.item_id == "1"));
        assert!(store
            .search("", "effects")
            .iter()
            .all(|item| matches!(item, SearchResult::Effect(_))));
        let effects = store.search("", "effects");
        for effect_type in ["videoEffect", "audioEffect", "videoTransition"] {
            assert!(effects.iter().any(|item| {
                matches!(item, SearchResult::Effect(entry) if entry.effect_type.id() == effect_type)
            }));
        }
        assert!(matches!(
            store.search("", "actions").as_slice(),
            [
                SearchResult::Action(Action::AddAdjustmentLayer),
                SearchResult::Action(Action::AddMarker),
                SearchResult::Action(Action::AddNamedMarker),
                SearchResult::Action(Action::GoToMarker),
                SearchResult::Action(Action::LockHorizontalAxis),
                SearchResult::Action(Action::RegisterAdjustmentLayer),
                SearchResult::Action(Action::RemoveAttributes)
            ]
        ));
        for query in ["rmat", "remove attributes"] {
            assert!(matches!(
                store.search(query, "actions").as_slice(),
                [SearchResult::Action(Action::RemoveAttributes)]
            ));
        }
        assert!(store.search("Gaussian Blur", "actions").is_empty());
        for action in Action::ALL {
            assert!(store.search(action.name(), "all").iter().any(
                |result| matches!(result, SearchResult::Action(found) if *found == action)
            ));
        }
        store.set_project_items(Vec::new());
        assert!(store.search("", "project").is_empty());
    }

    #[test]
    fn effect_types_and_catalog_ids_keep_their_wire_names() {
        for (json, expected) in [
            ("videoEffect", EffectType::VideoEffect),
            ("audioEffect", EffectType::AudioEffect),
            ("videoTransition", EffectType::VideoTransition),
            ("audioTransition", EffectType::AudioTransition),
            ("preset", EffectType::Preset),
            ("somethingNew", EffectType::Unknown),
        ] {
            let entry: EffectEntry = serde_json::from_value(serde_json::json!({
                "name": "Blur", "matchName": "blur", "effectType": json
            }))
            .unwrap();
            assert_eq!(entry.effect_type, expected);
            assert_eq!(EffectType::from(json), expected);
            if expected != EffectType::Unknown {
                assert_eq!(serde_json::to_value(expected).unwrap(), json);
                assert_eq!(expected.id(), json);
            }
        }
        for catalog in Catalog::ALL {
            assert_eq!(Catalog::from_id(catalog.id()), Some(catalog));
        }
        assert_eq!(
            Catalog::ALL.map(Catalog::id),
            ["all", "effects", "project", "library", "actions"]
        );
        assert_eq!(parent_bin_path("Footage / B-Roll"), Some("Footage"));
        assert_eq!(parent_bin_path("Footage"), None);
    }

    #[test]
    fn bin_drill_down_scopes_by_exact_bin_path_subtree() {
        let bin = |id: &str, name: &str, bin_path: &str| ProjectItemEntry {
            item_id: id.into(),
            name: name.into(),
            bin_path: bin_path.into(),
            kind: ProjectItemKind::Bin,
            offline: false,
            media_type: None,
            media_path: None,
        };
        let clip = |id: &str, name: &str, bin_path: &str| ProjectItemEntry {
            item_id: id.into(),
            name: name.into(),
            bin_path: bin_path.into(),
            kind: ProjectItemKind::Media,
            offline: false,
            media_type: None,
            media_path: None,
        };

        let root_bin = bin("b1", "Footage", "Project root");
        let sub_bin = bin("b2", "B-Roll", "Footage");
        let sibling_bin = bin("b3", "Footage Extra", "Project root");
        assert_eq!(root_bin.child_bin_path().as_deref(), Some("Footage"));
        assert_eq!(sub_bin.child_bin_path().as_deref(), Some("Footage / B-Roll"));
        assert_eq!(
            clip("c0", "Clip", "Footage").child_bin_path(),
            None
        );

        assert!(in_bin_scope("Footage", "Footage"));
        assert!(in_bin_scope("Footage / B-Roll", "Footage"));
        assert!(!in_bin_scope("Footage Extra", "Footage"));

        let mut store = CatalogStore::new_with_defaults();
        store.set_project_items(vec![
            root_bin,
            sub_bin,
            sibling_bin,
            clip("c1", "A-Cam.mov", "Footage"),
            clip("c2", "Street.mov", "Footage / B-Roll"),
            clip("c3", "Other.mov", "Footage Extra"),
        ]);
        assert!(store.contains_bin_path("Footage / B-Roll"));
        assert!(!store.contains_bin_path("Missing"));

        let in_footage = store.search_in_bin("", Catalog::Project, Some("Footage"));
        let names: Vec<_> = in_footage
            .iter()
            .map(|r| match r {
                SearchResult::ProjectItem(item) => item.name.as_str(),
                SearchResult::Effect(e) => e.name.as_str(),
                SearchResult::Action(action) => action.name(),
                SearchResult::Marker(marker) => marker.title(),
                SearchResult::LibraryFile(file) => file.name.as_str(),
            })
            .collect();
        assert_eq!(names, ["B-Roll", "A-Cam.mov", "Street.mov"]);

        let filtered = store.search_in_bin("street", Catalog::Project, Some("Footage"));
        assert!(matches!(
            filtered.as_slice(),
            [SearchResult::ProjectItem(item)] if item.item_id == "c2"
        ));
    }

    #[test]
    fn usage_boosts_only_the_applied_effect_type() {
        let entry = |name: &str, effect_type: &str| EffectEntry {
            name: name.into(),
            match_name: "shared".into(),
            effect_type: effect_type.into(),
            folder: String::new(),
        };
        let mut store = CatalogStore::new_with_defaults();
        store.set_catalog(vec![entry("Alpha", "videoEffect"), entry("Zeta", "audioEffect")]);
        store.record_usage(&entry("Zeta", "audioEffect"));
        let names: Vec<_> = store
            .search("", "effects")
            .into_iter()
            .map(|result| match result {
                SearchResult::Effect(effect) => effect.name.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(names, ["Zeta", "Alpha"]);
    }

    fn result_name(result: &SearchResult) -> &str {
        match result {
            SearchResult::Effect(effect) => &effect.name,
            SearchResult::ProjectItem(item) => &item.name,
            SearchResult::Action(action) => action.name(),
            SearchResult::Marker(marker) => marker.title(),
            SearchResult::LibraryFile(file) => &file.name,
        }
    }

    /// The default effects plus a project with a few named clips and many
    /// camera-card clips, whose long bin path used to attract scattered matches.
    fn relevance_store() -> CatalogStore {
        let clip = |id: String, name: &str, bin_path: &str| ProjectItemEntry {
            item_id: id,
            name: name.into(),
            bin_path: bin_path.into(),
            kind: ProjectItemKind::Media,
            offline: false,
            media_type: None,
            media_path: None,
        };
        let mut items: Vec<_> = [
            ("A-Cam_Interview_01.mov", "Footage / Interviews"),
            ("Interview.mp4", "Footage / Interviews"),
            ("Interview B-Roll Street.mp4", "Footage / B-Roll"),
            ("street_day_01.mov", "Footage / B-Roll"),
            ("Gaussian test render.mov", "Exports"),
            ("Blur Pass.mov", "Graphics / Blur Sharpen Archive"),
            ("Crossfade music.wav", "Audio / Music"),
            ("Café scene.mov", "Footage"),
        ]
        .into_iter()
        .enumerate()
        .map(|(i, (name, bin_path))| clip(i.to_string(), name, bin_path))
        .collect();
        items.extend((0..200).map(|i| {
            clip(
                format!("card{i}"),
                &format!("Clip_{i:04}_take{}.mov", i % 7),
                "Footage / Raw Cards / Card A",
            )
        }));
        let mut store = CatalogStore::new_with_defaults();
        store.set_project_items(items);
        store
    }

    #[test]
    fn the_intended_result_ranks_first() {
        let mut store = relevance_store();
        for (query, first) in [
            ("blur", "Blur Pass.mov"),
            ("gaussian", "Gaussian Blur"),
            ("blur gaussian", "Gaussian Blur"),
            ("gb", "Gaussian Blur"),
            ("dip bl", "Dip to Black"),
            ("dip to w", "Dip to White"),
            ("color", "Color Key"),
            ("color key", "Color Key"),
            ("crop", "Crop"),
            ("brightness contrast", "Brightness & Contrast"),
            ("brightness & contrast", "Brightness & Contrast"),
            ("single band", "Single-band Compressor"),
            ("noise", "Noise"),
            ("denoise", "DeNoise"),
            ("interview", "Interview.mp4"),
            ("a cam", "A-Cam_Interview_01.mov"),
            ("a-cam interview", "A-Cam_Interview_01.mov"),
            ("street day", "street_day_01.mov"),
            ("cafe", "Café scene.mov"),
            ("CROSS DIS", "Cross Dissolve"),
            ("  push  ", "Push"),
            ("rmat", "Remove Attributes"),
            ("go to marker", "Go to Marker"),
            ("(hls)", "Color Balance (HLS)"),
            ("3d", "Basic 3D"),
            ("clip 0042", "Clip_0042_take0.mov"),
        ] {
            let results = store.search(query, "all");
            assert_eq!(
                results.first().map(result_name),
                Some(first),
                "{query:?} ranked {:?}",
                results.iter().take(5).map(result_name).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn scattered_letters_and_near_misses_stay_out_of_results() {
        let mut store = relevance_store();
        for (query, expected) in [
            // Each of these once matched hundreds of clips through letters
            // spread across the name and the bin path.
            ("cafe", &["Café scene.mov"][..]),
            ("3d", &["Basic 3D"]),
            ("crop", &["Crop"]),
            ("gb", &["Gaussian Blur", "Single-band Compressor"]),
            ("push", &["Push"]),
            ("street day", &["street_day_01.mov"]),
            ("clip 0042", &["Clip_0042_take0.mov"]),
        ] {
            let results = store.search(query, "all");
            let names: Vec<_> = results.iter().map(result_name).collect();
            assert_eq!(names, expected, "{query:?}");
        }
    }

    #[test]
    fn typos_still_find_names_but_rank_below_clean_matches() {
        let mut store = relevance_store();
        for (query, first) in [
            ("guassian", "Gaussian Blur"),
            ("guassian blur", "Gaussian Blur"),
            ("interveiw", "Interview.mp4"),
            ("lumetir", "Lumetri Color"),
            ("compresor", "Multiband Compressor"),
            ("mosiac", "Mosaic"),
            ("disolve cross", "Cross Dissolve"),
        ] {
            let results = store.search(query, "all");
            assert_eq!(results.first().map(result_name), Some(first), "{query:?}");
        }
        let results = store.search("guassian", "all");
        let names: Vec<_> = results.iter().map(result_name).collect();
        assert_eq!(names, ["Gaussian Blur", "Gaussian test render.mov"]);
    }

    #[test]
    fn context_finds_kinds_folders_and_bins_below_name_matches() {
        let mut store = relevance_store();
        let transitions = store.search("transition", "effects");
        assert!(!transitions.is_empty());
        assert!(transitions.iter().all(
            |r| matches!(r, SearchResult::Effect(e) if e.effect_type.is_transition())
        ));
        let trans = store.search("trans", "effects");
        assert_eq!(trans.first().map(result_name), Some("Transform"));
        assert!(trans.iter().any(|r| result_name(r) == "Cross Dissolve"));

        let keying = store.search("keying", "effects");
        assert_eq!(keying.len(), 4);
        // The bin name finds its clips, after a clip whose own name matches.
        let interviews = store.search("interviews", "project");
        let names: Vec<_> = interviews.iter().map(result_name).collect();
        assert_eq!(
            names,
            ["Interview B-Roll Street.mp4", "Interview.mp4", "A-Cam_Interview_01.mov"]
        );
        assert_eq!(store.search("raw cards", "project").len(), 200);
    }

    #[test]
    fn usage_reorders_similar_matches_but_not_past_an_exact_match() {
        assert_eq!(usage_bonus(0), 0);
        assert!(usage_bonus(1) < usage_bonus(3));
        assert_eq!(usage_bonus(1_000), usage_bonus(u32::MAX));
        assert!(usage_bonus(u32::MAX) < 200);

        let mut store = relevance_store();
        let lumetri = EffectEntry {
            name: "Lumetri Color".into(),
            match_name: "AE.ADBE Lumetri".into(),
            effect_type: EffectType::VideoEffect,
            folder: String::new(),
        };
        for _ in 0..500 {
            store.record_usage(&lumetri);
        }
        let first = |store: &mut CatalogStore, query| {
            store.search(query, "all").first().map(result_name).map(String::from)
        };
        assert_eq!(first(&mut store, "color").as_deref(), Some("Lumetri Color"));
        assert_eq!(first(&mut store, "color key").as_deref(), Some("Color Key"));
        assert_eq!(first(&mut store, "color balance").as_deref(), Some("Color Balance (HLS)"));
    }

    fn library_file(path: &str, folder: &str) -> LibraryEntry {
        let path = std::path::PathBuf::from(path);
        let media_type = library::importable_media_type(&path).unwrap();
        LibraryEntry {
            name: path.file_name().unwrap().to_str().unwrap().to_owned(),
            key: library::path_key(path.to_str().unwrap()),
            path,
            folder: folder.into(),
            media_type,
        }
    }

    #[test]
    fn library_files_join_all_once_and_their_own_tab_always() {
        let mut store = CatalogStore::new_with_defaults();
        store.set_library_files(vec![
            library_file(r"D:\SFX\Boom.wav", "SFX"),
            library_file(r"D:\SFX\Whooshes\Whoosh_01.wav", "SFX / Whooshes"),
            library_file(r"D:\Memes\Bruh.mp4", "Memes"),
        ]);
        store.set_project_items(vec![ProjectItemEntry {
            item_id: "boom".into(),
            name: "Boom.wav".into(),
            bin_path: PROJECT_ROOT_BIN.into(),
            kind: ProjectItemKind::Media,
            offline: false,
            media_type: Some(MediaType::Audio),
            // Premiere's spelling of the same file.
            media_path: Some("d:/sfx/BOOM.wav".into()),
        }]);

        // All: the imported file shows once, as its project clip.
        let all = store.search("boom", "all");
        assert!(matches!(all.as_slice(), [SearchResult::ProjectItem(item)] if item.item_id == "boom"));
        // Library: every tracked file, and the imported one stands for its clip.
        let library = store.search("", "library");
        assert_eq!(library.iter().map(result_name).collect::<Vec<_>>(), ["Boom.wav", "Bruh.mp4", "Whoosh_01.wav"]);
        assert!(matches!(store.resolve(&library[0]), SearchResult::ProjectItem(item) if item.item_id == "boom"));
        assert!(matches!(store.resolve(&library[1]), SearchResult::LibraryFile(_)));
        // Folders and the catalog name are context.
        assert_eq!(store.search("whooshes", "all").iter().map(result_name).collect::<Vec<_>>(), ["Whoosh_01.wav"]);
        assert_eq!(store.search("library", "library").len(), 3);
        assert!(store.search("", "effects").iter().all(|r| !matches!(r, SearchResult::LibraryFile(_))));
        assert!(store.search("boom", "project").iter().all(|r| !matches!(r, SearchResult::LibraryFile(_))));
        assert_eq!(store.counts().2, 3);
        assert!(store.library_file(&library::path_key(r"d:\memes\bruh.MP4")).is_some());

        // Without a path from Premiere, nothing can be matched, so both show.
        store.set_project_items(vec![ProjectItemEntry {
            item_id: "boom".into(),
            name: "Boom.wav".into(),
            bin_path: PROJECT_ROOT_BIN.into(),
            kind: ProjectItemKind::Media,
            offline: false,
            media_type: Some(MediaType::Audio),
            media_path: None,
        }]);
        assert_eq!(store.search("boom", "all").len(), 2);
    }

    #[test]
    fn library_files_rank_after_equal_matches() {
        let mut store = CatalogStore::new_with_defaults();
        store.set_library_files(vec![library_file(r"D:\SFX\Crop.wav", "SFX")]);
        let names: Vec<_> = store.search("crop", "all").iter().map(result_name).map(String::from).collect();
        assert_eq!(names.first().map(String::as_str), Some("Crop"));
        assert!(names.contains(&"Crop.wav".to_string()));
    }

    #[test]
    fn library_lookup_follows_each_index_update() {
        let mut store = CatalogStore::new_with_defaults();
        let key = |path: &str| library::path_key(path);
        store.set_library_files(vec![library_file(r"D:\SFX\Boom.wav", "SFX"), library_file(r"D:\SFX\Clap.wav", "SFX")]);
        assert_eq!(store.library_file(&key("d:/sfx/CLAP.wav")).map(|f| f.name.clone()).as_deref(), Some("Clap.wav"));

        // Boom.wav was deleted and Swish.wav added: the lookup follows the new list.
        store.set_library_files(vec![library_file(r"D:\SFX\Clap.wav", "SFX"), library_file(r"D:\SFX\Swish.wav", "SFX")]);
        assert!(store.library_file(&key(r"D:\SFX\Boom.wav")).is_none());
        assert_eq!(store.library_file(&key(r"D:\SFX\Clap.wav")).map(|f| f.name.clone()).as_deref(), Some("Clap.wav"));
        assert_eq!(store.library_file(&key(r"D:\SFX\Swish.wav")).map(|f| f.name.clone()).as_deref(), Some("Swish.wav"));

        store.set_library_files(Vec::new());
        assert!(store.library_file(&key(r"D:\SFX\Clap.wav")).is_none());
    }

    #[test]
    fn markers_rank_title_matches_first_and_otherwise_keep_timeline_order() {
        let marker = |name: &str, comments: &str, ticks: u64| {
            Arc::new(
                serde_json::from_value::<SequenceMarker>(serde_json::json!({
                    "sequenceId": "s", "sequenceName": "Selects", "name": name,
                    "comments": comments, "ticks": ticks.to_string(),
                }))
                .unwrap(),
            )
        };
        let markers = [
            marker("Music cue", "fade the intro song", 0),
            marker("Intro", "", 254_016_000_000),
            marker("Outro", "", 508_032_000_000),
            marker("Intro b", "", 762_048_000_000),
        ];
        let mut store = CatalogStore::new_with_defaults();
        let titles = |results: Vec<SearchResult>| -> Vec<String> {
            results.iter().map(|r| result_name(r).to_string()).collect()
        };
        assert_eq!(titles(store.search_markers("", &markers)), ["Music cue", "Intro", "Outro", "Intro b"]);
        assert_eq!(titles(store.search_markers("intro", &markers)), ["Intro", "Intro b", "Music cue"]);
        assert_eq!(titles(store.search_markers("selects", &markers)), ["Music cue", "Intro", "Outro", "Intro b"]);
        assert!(store.search_markers("zzz", &markers).is_empty());
    }
}
