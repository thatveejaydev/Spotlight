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

    pub fn matches_filter(&self, filter: &str) -> bool {
        match filter {
            "all" | "" => true,
            "videoEffect" => self.effect_type == "videoEffect",
            "audioEffect" => self.effect_type == "audioEffect",
            "transition" => {
                self.effect_type == "videoTransition" || self.effect_type == "audioTransition"
            }
            _ => true,
        }
    }
}

pub struct CatalogStore {
    items: Vec<EffectEntry>,
    usage_counts: HashMap<String, u32>,
    matcher: Matcher,
}

impl CatalogStore {
    pub fn new_with_defaults() -> Self {
        Self {
            items: default_premiere_effects(),
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

    pub fn search(&mut self, query: &str, filter: &str) -> Vec<EffectEntry> {
        let trimmed = query.trim();

        if trimmed.is_empty() {
            let mut filtered: Vec<EffectEntry> = self
                .items
                .iter()
                .filter(|item| item.matches_filter(filter))
                .cloned()
                .collect();

            filtered.sort_by(|a, b| {
                let ua = self.usage_counts.get(&a.match_name).copied().unwrap_or(0);
                let ub = self.usage_counts.get(&b.match_name).copied().unwrap_or(0);
                ub.cmp(&ua)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
            return filtered;
        }

        let pattern = Pattern::new(
            trimmed,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        let q_lower = trimmed.to_lowercase();
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, EffectEntry)> = Vec::new();

        for item in &self.items {
            if !item.matches_filter(filter) {
                continue;
            }
            let hay = format!("{} {}", item.name, item.folder);
            let utf32 = Utf32Str::new(&hay, &mut buf);
            if let Some(mut score) = pattern.score(utf32, &mut self.matcher) {
                let name_lower = item.name.to_lowercase();
                if name_lower == q_lower {
                    score += 500;
                } else if name_lower.starts_with(&q_lower) {
                    score += 200;
                } else if name_lower.contains(&q_lower) {
                    score += 80;
                }
                let usage = self.usage_counts.get(&item.match_name).copied().unwrap_or(0);
                score += usage.min(50) * 15;
                scored.push((score, item.clone()));
            }
        }

        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.name.len().cmp(&b.1.name.len()))
                .then_with(|| a.1.name.cmp(&b.1.name))
        });

        scored.into_iter().map(|(_, item)| item).collect()
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
