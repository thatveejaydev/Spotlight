use nucleo_matcher::{
    pattern::{AtomKind, CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectEntry {
    pub name: String,
    pub match_name: String,
    /// "videoEffect" | "audioEffect" | "videoTransition" | "audioTransition"
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
            ProjectItemKind::Bin => Some("Bins can't be placed on the timeline."),
            ProjectItemKind::ActiveSequence => Some("A sequence can't nest into itself."),
            ProjectItemKind::Other => Some("This project item can't be placed on the timeline."),
            _ if self.offline => Some("Relink the offline media before placing it."),
            _ => None,
        }
    }
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
    Effect(EffectEntry),
    ProjectItem(ProjectItemEntry),
}

#[derive(Clone, Copy)]
enum CatalogEntry<'a> {
    Effect(&'a EffectEntry),
    ProjectItem(&'a ProjectItemEntry),
}

impl SearchResult {
    pub fn same_item(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Effect(a), Self::Effect(b)) => {
                a.effect_type == b.effect_type && a.match_name == b.match_name
            }
            (Self::ProjectItem(a), Self::ProjectItem(b)) => a.item_id == b.item_id,
            _ => false,
        }
    }
}

impl CatalogEntry<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Effect(entry) => &entry.name,
            Self::ProjectItem(entry) => &entry.name,
        }
    }

    fn folder(&self) -> &str {
        match self {
            Self::Effect(entry) => &entry.folder,
            Self::ProjectItem(entry) => &entry.bin_path,
        }
    }

    fn to_result(self) -> SearchResult {
        match self {
            Self::Effect(entry) => SearchResult::Effect(entry.clone()),
            Self::ProjectItem(entry) => SearchResult::ProjectItem(entry.clone()),
        }
    }
}

impl EffectEntry {
    pub fn category_short(&self) -> &'static str {
        match self.effect_type.as_str() {
            "videoEffect" => "Video Effect",
            "audioEffect" => "Audio Effect",
            "videoTransition" => "Video Transition",
            "audioTransition" => "Audio Transition",
            _ => "Effect",
        }
    }

    fn is_supported(&self) -> bool {
        matches!(self.effect_type.as_str(), "videoEffect" | "audioEffect" | "videoTransition")
    }
}

pub struct CatalogStore {
    items: Vec<EffectEntry>,
    project_items: Vec<ProjectItemEntry>,
    usage_counts: HashMap<String, u32>,
    matcher: Matcher,
}

impl CatalogStore {
    pub fn new_with_defaults() -> Self {
        Self {
            items: default_premiere_effects(),
            project_items: Vec::new(),
            usage_counts: HashMap::new(),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn set_catalog(&mut self, mut new_items: Vec<EffectEntry>) {
        if new_items.is_empty() {
            return;
        }
        new_items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        new_items.dedup_by(|a, b| {
            a.effect_type == b.effect_type && a.match_name == b.match_name && a.name == b.name
        });
        self.items = new_items;
    }

    pub fn record_usage(&mut self, match_name: &str) {
        let count = self.usage_counts.entry(match_name.to_string()).or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn set_project_items(&mut self, items: Vec<ProjectItemEntry>) {
        self.project_items = items;
    }

    pub fn search(&mut self, query: &str, catalog: &str) -> Vec<SearchResult> {
        let trimmed = query.trim();
        let effects = self
            .items
            .iter()
            .filter(|item| catalog != "project" && item.is_supported())
            .map(CatalogEntry::Effect);
        let project_items = self
            .project_items
            .iter()
            .filter(|_| catalog != "effects")
            .map(CatalogEntry::ProjectItem);
        let entries = effects.chain(project_items);
        let usage = |item: &CatalogEntry| match item {
            CatalogEntry::Effect(entry) => self
                .usage_counts
                .get(&entry.match_name)
                .copied()
                .unwrap_or(0),
            CatalogEntry::ProjectItem(_) => 0,
        };

        if trimmed.is_empty() {
            let mut filtered: Vec<_> = entries.collect();

            filtered.sort_by(|a, b| {
                usage(b)
                    .cmp(&usage(a))
                    .then_with(|| a.name().to_lowercase().cmp(&b.name().to_lowercase()))
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
        let mut buf = Vec::new();
        let mut scored = Vec::new();

        for item in entries {
            let hay = format!("{} {}", item.name(), item.folder());
            let utf32 = Utf32Str::new(&hay, &mut buf);
            if let Some(mut score) = pattern.score(utf32, &mut self.matcher) {
                let name_lower = item.name().to_lowercase();
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
        store.set_project_items(Vec::new());
        assert!(store.search("", "project").is_empty());
    }
}
