use crate::actions::{markers::SequenceMarker, Action};
use nucleo_matcher::{
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap, sync::Arc};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectEntry {
    pub name: String,
    pub match_name: String,
    /// "videoEffect" | "audioEffect" | "videoTransition" | "audioTransition" | "preset"
    pub effect_type: String,
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
        Some(if self.bin_path == "Project root" {
            self.name.clone()
        } else {
            format!("{} / {}", self.bin_path, self.name)
        })
    }
}

fn in_bin_scope(item_bin_path: &str, scope: &str) -> bool {
    item_bin_path == scope
        || item_bin_path
            .strip_prefix(scope)
            .is_some_and(|rest| rest.starts_with(" / "))
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
}

struct IndexedEffect {
    entry: Arc<EffectEntry>,
    name_lower: String,
    haystack: String,
    supported: bool,
}

impl IndexedEffect {
    fn new(entry: EffectEntry) -> Self {
        let name_lower = entry.name.to_lowercase();
        let haystack = format!("{} {}", entry.name, entry.folder);
        let supported = entry.is_supported();
        Self {
            entry: Arc::new(entry),
            name_lower,
            haystack,
            supported,
        }
    }
}

struct IndexedProjectItem {
    entry: Arc<ProjectItemEntry>,
    name_lower: String,
    haystack: String,
}

impl IndexedProjectItem {
    fn new(entry: ProjectItemEntry) -> Self {
        let name_lower = entry.name.to_lowercase();
        let haystack = format!("{} {}", entry.name, entry.bin_path);
        Self {
            entry: Arc::new(entry),
            name_lower,
            haystack,
        }
    }
}

#[derive(Clone, Copy)]
enum CatalogEntry<'a> {
    Effect(&'a IndexedEffect),
    ProjectItem(&'a IndexedProjectItem),
    Action(Action),
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
            _ => false,
        }
    }
}

impl CatalogEntry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Effect(item) => &item.entry.name,
            Self::ProjectItem(item) => &item.entry.name,
            Self::Action(action) => action.name(),
        }
    }

    fn name_lower(&self) -> Cow<'_, str> {
        match self {
            Self::Effect(item) => Cow::Borrowed(&item.name_lower),
            Self::ProjectItem(item) => Cow::Borrowed(&item.name_lower),
            Self::Action(action) => Cow::Owned(action.name().to_lowercase()),
        }
    }

    fn folder(&self) -> &str {
        match self {
            Self::Effect(item) => &item.entry.folder,
            Self::ProjectItem(item) => &item.entry.bin_path,
            Self::Action(_) => "",
        }
    }

    fn haystack(&self) -> &str {
        match self {
            Self::Effect(item) => &item.haystack,
            Self::ProjectItem(item) => &item.haystack,
            Self::Action(action) => action.name(),
        }
    }

    fn to_result(self) -> SearchResult {
        match self {
            Self::Effect(item) => SearchResult::Effect(Arc::clone(&item.entry)),
            Self::ProjectItem(item) => SearchResult::ProjectItem(Arc::clone(&item.entry)),
            Self::Action(action) => SearchResult::Action(action),
        }
    }
}

impl EffectEntry {
    pub fn is_preset(&self) -> bool {
        self.effect_type == "preset"
    }

    pub fn category_short(&self) -> &'static str {
        match self.effect_type.as_str() {
            "videoEffect" => "Video FX",
            "audioEffect" => "Audio FX",
            "videoTransition" => "Video Trans",
            "audioTransition" => "Audio Transition",
            "preset" => "Preset",
            _ => "Effect",
        }
    }

    fn is_supported(&self) -> bool {
        matches!(
            self.effect_type.as_str(),
            "videoEffect" | "audioEffect" | "videoTransition" | "preset"
        )
    }
}

pub struct CatalogStore {
    items: Vec<IndexedEffect>,
    presets: Vec<IndexedEffect>,
    project_items: Vec<IndexedProjectItem>,
    usage_counts: HashMap<String, u32>,
    matcher: Matcher,
    utf32_buf: Vec<char>,
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
            usage_counts: HashMap::new(),
            matcher: Matcher::new(Config::DEFAULT),
            utf32_buf: Vec::with_capacity(128),
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

    pub fn record_usage(&mut self, match_name: &str) {
        let count = self.usage_counts.entry(match_name.to_string()).or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn set_project_items(&mut self, items: Vec<ProjectItemEntry>) {
        self.project_items = items
            .into_iter()
            .map(IndexedProjectItem::new)
            .collect();
    }

    pub fn counts(&self) -> (usize, usize) {
        (self.items.len() + self.presets.len(), self.project_items.len())
    }

    pub fn contains_bin_path(&self, bin_path: &str) -> bool {
        self.project_items
            .iter()
            .any(|item| item.entry.child_bin_path().as_deref() == Some(bin_path))
    }

    #[cfg(test)]
    pub fn search(&mut self, query: &str, catalog: &str) -> Vec<SearchResult> {
        self.search_in_bin(query, catalog, None)
    }

    pub fn search_markers(&mut self, query: &str, markers: &[Arc<SequenceMarker>]) -> Vec<SearchResult> {
        let query = query.trim();
        let pattern = Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
        markers.iter()
            .filter(|marker| {
                if query.is_empty() {
                    return true;
                }
                let text = format!("{} {} {} {}", marker.title(), marker.sequence_name, marker.comments, marker.position());
                pattern.score(Utf32Str::new(&text, &mut self.utf32_buf), &mut self.matcher).is_some()
            })
            .cloned()
            .map(SearchResult::Marker)
            .collect()
    }

    pub fn search_in_bin(
        &mut self,
        query: &str,
        catalog: &str,
        bin_scope: Option<&str>,
    ) -> Vec<SearchResult> {
        let trimmed = query.trim();
        let include_effects = matches!(catalog, "all" | "effects") && bin_scope.is_none();
        let effects = self
            .items
            .iter()
            .chain(&self.presets)
            .filter(move |item| include_effects && item.supported)
            .map(CatalogEntry::Effect);
        let include_project = matches!(catalog, "all" | "project");
        let project_items = self
            .project_items
            .iter()
            .filter(move |item| {
                include_project
                    && bin_scope.is_none_or(|scope| in_bin_scope(&item.entry.bin_path, scope))
            })
            .map(CatalogEntry::ProjectItem);
        let actions = Action::ALL
            .into_iter()
            .filter(|_| matches!(catalog, "all" | "actions") && bin_scope.is_none())
            .map(CatalogEntry::Action);
        let entries = effects.chain(project_items).chain(actions);
        let usage = |item: &CatalogEntry| match item {
            CatalogEntry::Effect(indexed) => self
                .usage_counts
                .get(&indexed.entry.match_name)
                .copied()
                .unwrap_or(0),
            CatalogEntry::ProjectItem(_) | CatalogEntry::Action(_) => 0,
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
                    .then_with(|| usage(b).cmp(&usage(a)))
                    .then_with(|| a.name_lower().cmp(&b.name_lower()))
                    .then_with(|| a.folder().cmp(b.folder()))
            });
            return filtered.into_iter().map(CatalogEntry::to_result).collect();
        }

        let pattern = Pattern::new(
            trimmed,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let q_lower = trimmed.to_lowercase();
        let mut scored = Vec::new();

        for item in entries {
            let utf32 = Utf32Str::new(item.haystack(), &mut self.utf32_buf);
            if let Some(mut score) = pattern.score(utf32, &mut self.matcher) {
                let name_lower = item.name_lower();
                if name_lower == q_lower {
                    score += 500;
                } else if name_lower.starts_with(&q_lower) {
                    score += 200;
                } else if name_lower.contains(&q_lower) {
                    score += 80;
                }
                score += usage(&item).min(50) * 15;
                scored.push((score, item));
            }
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.name().len().cmp(&b.1.name().len()))
                .then_with(|| a.1.name().cmp(b.1.name()))
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
            effect_type: effect_type.to_string(),
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
            },
            ProjectItemEntry {
                item_id: "2".into(),
                name: "Gaussian Blur".into(),
                bin_path: "Drafts".into(),
                kind: ProjectItemKind::Sequence,
                offline: false,
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
                matches!(item, SearchResult::Effect(entry) if entry.effect_type == effect_type)
            }));
        }
        assert!(matches!(
            store.search("", "actions").as_slice(),
            [
                SearchResult::Action(Action::AddAdjustmentLayer),
                SearchResult::Action(Action::AddMarker),
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
    fn bin_drill_down_scopes_by_exact_bin_path_subtree() {
        let bin = |id: &str, name: &str, bin_path: &str| ProjectItemEntry {
            item_id: id.into(),
            name: name.into(),
            bin_path: bin_path.into(),
            kind: ProjectItemKind::Bin,
            offline: false,
        };
        let clip = |id: &str, name: &str, bin_path: &str| ProjectItemEntry {
            item_id: id.into(),
            name: name.into(),
            bin_path: bin_path.into(),
            kind: ProjectItemKind::Media,
            offline: false,
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

        let in_footage = store.search_in_bin("", "project", Some("Footage"));
        let names: Vec<_> = in_footage
            .iter()
            .map(|r| match r {
                SearchResult::ProjectItem(item) => item.name.as_str(),
                SearchResult::Effect(e) => e.name.as_str(),
                SearchResult::Action(action) => action.name(),
                SearchResult::Marker(marker) => marker.title(),
            })
            .collect();
        assert_eq!(names, ["B-Roll", "A-Cam.mov", "Street.mov"]);

        let filtered = store.search_in_bin("street", "project", Some("Footage"));
        assert!(matches!(
            filtered.as_slice(),
            [SearchResult::ProjectItem(item)] if item.item_id == "c2"
        ));
    }
}
