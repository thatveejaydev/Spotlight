//! Whether a result can run in Premiere right now, and the reason shown when it can't.
//!
//! The plugin reports facts ([`PremiereContext`]) and these rules decide. A fact
//! that is unknown never blocks a result, so a missing or late reply leaves every
//! row enabled. The plugin still checks each command when it runs.

use crate::{
    actions::Action,
    bridge::MediaCatalog,
    catalog::{EffectType, ProjectItemEntry, ProjectItemKind, SearchResult},
    protocol::PremiereContext,
};

const NO_PROJECT: &str = "Open a project first.";
const NO_SEQUENCE: &str = "Open a sequence first.";
const NO_SELECTION: &str = "Select clips in the Timeline first.";
const NO_VIDEO: &str = "Select a video clip first.";
const NO_AUDIO: &str = "Select an audio clip first.";
const NO_PLACEMENT_SEQUENCE: &str = "Open a sequence to place media.";
const NEEDS_25_6: &str = "Requires Premiere Pro 25.6 or later.";
const NOT_REGISTERED: &str = "Run Register Adjustment Layer first.";
const NOT_ADJUSTMENT_LAYER: &str = "Select one adjustment layer in the Timeline.";

/// The longest sequence name the footer shows before eliding it.
const SUMMARY_NAME_CHARS: usize = 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Ready,
    Blocked(&'static str),
}

impl Availability {
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Blocked(reason) => Some(reason),
        }
    }

    fn from_check(check: Result<(), &'static str>) -> Self {
        check.map_or_else(Self::Blocked, |()| Self::Ready)
    }
}

/// The facts the rules read. `None` means unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    project: Option<bool>,
    sequence: Option<bool>,
    /// Whether media can be placed. The project list also reports the sequence, so
    /// this is known even before the plugin answers.
    placement_sequence: Option<bool>,
    selected: Option<u32>,
    video: Option<u32>,
    audio: Option<u32>,
    single_adjustment_layer: Option<bool>,
    adjustment_registered: Option<bool>,
    /// Premiere Pro 25.6 or later.
    modern_host: Option<bool>,
}

impl Facts {
    pub fn new(context: Option<&PremiereContext>, media: Option<&MediaCatalog>) -> Self {
        let mut facts = Self::default();
        if let Some(context) = context {
            let project = context.project_id.is_some();
            facts.project = Some(project);
            facts.sequence = Some(project && context.sequence_id.is_some());
            if let Some(selection) = context.selection.as_ref().filter(|_| facts.sequence == Some(true)) {
                facts.selected = Some(selection.total);
                facts.video = selection.video;
                facts.audio = selection.audio;
                facts.single_adjustment_layer = selection.single_adjustment_layer;
            }
            facts.adjustment_registered = context.adjustment_layer_registered;
            facts.modern_host = context.host_version.as_deref().and_then(supports_25_6);
        }
        // The project list is cleared without a project, so only a listed project tells.
        let listed_sequence = media
            .filter(|media| media.project_id.is_some())
            .map(|media| media.sequence_id.is_some());
        facts.placement_sequence = facts.sequence.or(listed_sequence);
        facts
    }
}

/// Whether `Enter` (or a click) on `result` can run.
pub fn primary(result: &SearchResult, facts: &Facts) -> Availability {
    Availability::from_check(match result {
        SearchResult::Effect(entry) => effect(entry.effect_type, facts),
        SearchResult::ProjectItem(entry) => placement(entry, facts),
        SearchResult::Action(action) => action_check(*action, facts),
        SearchResult::Marker(_) => Ok(()),
    })
}

/// Whether the alternate action (`Shift+Enter` by default) on `result` can run.
pub fn alternate(result: &SearchResult, facts: &Facts) -> Availability {
    match result {
        SearchResult::ProjectItem(entry) => Availability::from_check(source_monitor(entry, facts)),
        _ => primary(result, facts),
    }
}

/// A short description of the active sequence and selection for the footer, or
/// empty while the plugin hasn't answered.
pub fn summary(context: Option<&PremiereContext>) -> String {
    let Some(context) = context else {
        return String::new();
    };
    if context.project_id.is_none() {
        return "No project".into();
    }
    if context.sequence_id.is_none() {
        return "No sequence".into();
    }
    let name = context
        .sequence_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Sequence");
    let name = if name.chars().count() > SUMMARY_NAME_CHARS {
        let short: String = name.chars().take(SUMMARY_NAME_CHARS - 1).collect();
        format!("{}…", short.trim_end())
    } else {
        name.to_string()
    };
    let Some(selection) = &context.selection else {
        return name;
    };
    match (selection.total, selection.video, selection.audio) {
        (0, _, _) => format!("{name} · No selection"),
        (_, Some(video), Some(audio)) if video + audio > 0 => {
            let parts: Vec<String> = [(video, "video"), (audio, "audio")]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, label)| format!("{count} {label}"))
                .collect();
            format!("{name} · {}", parts.join(", "))
        }
        (total, _, _) => format!("{name} · {total} selected"),
    }
}

/// Blocks with `reason` only when `fact` is known to be false.
fn require(fact: Option<bool>, reason: &'static str) -> Result<(), &'static str> {
    if fact == Some(false) {
        Err(reason)
    } else {
        Ok(())
    }
}

/// Blocks with `reason` only when `count` is known to be zero.
fn some_selected(count: Option<u32>, reason: &'static str) -> Result<(), &'static str> {
    require(count.map(|count| count > 0), reason)
}

fn timeline(facts: &Facts) -> Result<(), &'static str> {
    require(facts.project, NO_PROJECT)?;
    require(facts.sequence, NO_SEQUENCE)
}

fn effect(kind: EffectType, facts: &Facts) -> Result<(), &'static str> {
    let (count, reason) = match kind {
        EffectType::VideoEffect | EffectType::VideoTransition => (facts.video, NO_VIDEO),
        EffectType::AudioEffect | EffectType::AudioTransition => (facts.audio, NO_AUDIO),
        // Presets open the Effects panel instead of applying.
        EffectType::Preset | EffectType::Unknown => return Ok(()),
    };
    timeline(facts)?;
    some_selected(facts.selected, NO_SELECTION)?;
    some_selected(count, reason)
}

fn placement(entry: &ProjectItemEntry, facts: &Facts) -> Result<(), &'static str> {
    // Enter on a bin opens it.
    if entry.kind == ProjectItemKind::Bin {
        return Ok(());
    }
    if let Some(message) = entry.placement_error() {
        return Err(message);
    }
    require(facts.placement_sequence, NO_PLACEMENT_SEQUENCE)
}

fn source_monitor(entry: &ProjectItemEntry, facts: &Facts) -> Result<(), &'static str> {
    if let Some(message) = entry.source_monitor_error() {
        return Err(message);
    }
    require(facts.modern_host, NEEDS_25_6)
}

fn action_check(action: Action, facts: &Facts) -> Result<(), &'static str> {
    match action {
        Action::AddAdjustmentLayer => {
            require(facts.modern_host, NEEDS_25_6)?;
            timeline(facts)?;
            some_selected(facts.selected, NO_VIDEO)?;
            some_selected(facts.video, NO_VIDEO)?;
            require(facts.adjustment_registered, NOT_REGISTERED)
        }
        Action::RegisterAdjustmentLayer => {
            require(facts.modern_host, NEEDS_25_6)?;
            timeline(facts)?;
            require(facts.single_adjustment_layer, NOT_ADJUSTMENT_LAYER)
        }
        Action::RemoveAttributes => {
            require(facts.modern_host, NEEDS_25_6)?;
            timeline(facts)?;
            some_selected(facts.selected, NO_SELECTION)
        }
        Action::AddMarker => timeline(facts),
        Action::GoToMarker => require(facts.project, NO_PROJECT),
        Action::LockHorizontalAxis => Ok(()),
    }
}

/// Whether a Premiere version string such as "25.6.1" is 25.6 or later.
fn supports_25_6(version: &str) -> Option<bool> {
    let mut parts = version.trim().split('.').map(|part| part.trim().parse::<u32>().ok());
    let major = parts.next()??;
    let minor = parts.next().flatten().unwrap_or(0);
    Some((major, minor) >= (25, 6))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{catalog::EffectEntry, protocol::SelectionSummary};
    use std::sync::Arc;

    fn context(selection: Option<SelectionSummary>) -> PremiereContext {
        PremiereContext {
            project_id: Some("project".into()),
            sequence_id: Some("sequence".into()),
            sequence_name: Some("Sequence 01".into()),
            selection,
            adjustment_layer_registered: Some(true),
            host_version: Some("25.6.0".into()),
        }
    }

    fn selected(total: u32, video: u32, audio: u32) -> Option<SelectionSummary> {
        Some(SelectionSummary {
            total,
            video: Some(video),
            audio: Some(audio),
            single_adjustment_layer: Some(false),
        })
    }

    fn facts(context: &PremiereContext) -> Facts {
        Facts::new(Some(context), None)
    }

    fn effect_result(kind: &str) -> SearchResult {
        SearchResult::Effect(Arc::new(EffectEntry {
            name: "Effect".into(),
            match_name: "effect".into(),
            effect_type: kind.into(),
            folder: String::new(),
        }))
    }

    fn item(kind: ProjectItemKind, offline: bool) -> SearchResult {
        SearchResult::ProjectItem(Arc::new(ProjectItemEntry {
            item_id: "item".into(),
            name: "Item".into(),
            bin_path: "Footage".into(),
            kind,
            offline,
        }))
    }

    fn action(action: Action) -> SearchResult {
        SearchResult::Action(action)
    }

    #[test]
    fn unknown_facts_never_block() {
        let unknown = Facts::new(None, None);
        let everything = [
            effect_result("videoEffect"),
            effect_result("audioEffect"),
            effect_result("videoTransition"),
            effect_result("audioTransition"),
            effect_result("preset"),
            item(ProjectItemKind::Media, false),
            item(ProjectItemKind::Sequence, false),
            item(ProjectItemKind::Bin, false),
        ]
        .into_iter()
        .chain(Action::ALL.map(action));
        for result in everything {
            assert_eq!(primary(&result, &unknown), Availability::Ready);
        }
        // A context with every field unknown is no different.
        let empty = PremiereContext {
            project_id: Some("project".into()),
            sequence_id: Some("sequence".into()),
            ..PremiereContext::default()
        };
        for id in Action::ALL {
            assert_eq!(primary(&action(id), &facts(&empty)), Availability::Ready, "{}", id.name());
        }
    }

    #[test]
    fn effects_need_a_sequence_and_a_matching_selection() {
        let no_project = PremiereContext::default();
        let no_sequence = PremiereContext { sequence_id: None, ..context(None) };
        let nothing = context(selected(0, 0, 0));
        let audio_only = context(selected(1, 0, 1));
        let video_only = context(selected(2, 2, 0));
        let unclassified = context(Some(SelectionSummary { total: 2, ..SelectionSummary::default() }));
        for (kind, cases) in [
            ("videoEffect", [NO_PROJECT, NO_SEQUENCE, NO_SELECTION, NO_VIDEO, "", ""]),
            ("videoTransition", [NO_PROJECT, NO_SEQUENCE, NO_SELECTION, NO_VIDEO, "", ""]),
            ("audioEffect", [NO_PROJECT, NO_SEQUENCE, NO_SELECTION, "", NO_AUDIO, ""]),
            ("audioTransition", [NO_PROJECT, NO_SEQUENCE, NO_SELECTION, "", NO_AUDIO, ""]),
        ] {
            let contexts = [&no_project, &no_sequence, &nothing, &audio_only, &video_only, &unclassified];
            for (context, expected) in contexts.into_iter().zip(cases) {
                let reason = primary(&effect_result(kind), &facts(context)).reason().unwrap_or("");
                assert_eq!(reason, expected, "{kind} with {context:?}");
            }
        }
        for context in [no_project, nothing] {
            assert_eq!(primary(&effect_result("preset"), &facts(&context)), Availability::Ready);
        }
    }

    #[test]
    fn media_placement_uses_the_item_and_either_sequence_source() {
        let video = context(selected(0, 0, 0));
        let blocked = |result: &SearchResult, facts: &Facts| primary(result, facts).reason();
        assert_eq!(blocked(&item(ProjectItemKind::Media, false), &facts(&video)), None);
        assert_eq!(blocked(&item(ProjectItemKind::Bin, false), &facts(&video)), None);
        assert!(blocked(&item(ProjectItemKind::Media, true), &facts(&video)).unwrap().contains("Relink"));
        assert!(blocked(&item(ProjectItemKind::ActiveSequence, false), &facts(&video))
            .unwrap()
            .contains("nest"));
        assert!(blocked(&item(ProjectItemKind::Other, false), &facts(&video)).is_some());

        let media = |sequence: Option<&str>| MediaCatalog {
            peer_id: 1,
            project_id: Some("project".into()),
            sequence_id: sequence.map(Into::into),
            items: Vec::new(),
        };
        let clip = item(ProjectItemKind::Media, false);
        let listed = |catalog: &MediaCatalog| Facts::new(None, Some(catalog));
        assert_eq!(blocked(&clip, &listed(&media(None))), Some(NO_PLACEMENT_SEQUENCE));
        assert_eq!(blocked(&clip, &listed(&media(Some("sequence")))), None);
        // The live context wins over the project list.
        let closed = PremiereContext { sequence_id: None, ..context(None) };
        assert_eq!(
            blocked(&clip, &Facts::new(Some(&closed), Some(&media(Some("sequence"))))),
            Some(NO_PLACEMENT_SEQUENCE)
        );
        // A cleared project list says nothing.
        let cleared = MediaCatalog { project_id: None, ..media(None) };
        assert_eq!(blocked(&clip, &listed(&cleared)), None);
        // The project list alone never blocks effects or actions.
        assert_eq!(blocked(&effect_result("videoEffect"), &listed(&media(None))), None);
    }

    #[test]
    fn source_monitor_needs_a_clip_and_premiere_25_6() {
        let modern = context(None);
        let old = PremiereContext { host_version: Some("25.5.2".into()), ..context(None) };
        let clip = item(ProjectItemKind::Media, false);
        assert_eq!(alternate(&clip, &facts(&modern)), Availability::Ready);
        assert_eq!(alternate(&clip, &facts(&old)), Availability::Blocked(NEEDS_25_6));
        assert!(alternate(&item(ProjectItemKind::Bin, false), &facts(&modern))
            .reason()
            .unwrap()
            .contains("Bins"));
        // An offline clip still opens in the Source Monitor, and the active sequence too.
        assert_eq!(alternate(&item(ProjectItemKind::Media, true), &facts(&modern)), Availability::Ready);
        assert_eq!(
            alternate(&item(ProjectItemKind::ActiveSequence, false), &facts(&modern)),
            Availability::Ready
        );
        // Effects use the same rule for both actions.
        let none = context(selected(0, 0, 0));
        assert_eq!(
            alternate(&effect_result("videoEffect"), &facts(&none)),
            primary(&effect_result("videoEffect"), &facts(&none))
        );
    }

    #[test]
    fn actions_check_their_own_requirements() {
        let reason = |action_id: Action, context: &PremiereContext| {
            primary(&action(action_id), &facts(context)).reason()
        };
        let none = context(selected(0, 0, 0));
        let video = context(selected(2, 2, 0));
        let audio = context(selected(1, 0, 1));
        let layer = context(Some(SelectionSummary {
            total: 1,
            video: Some(1),
            audio: Some(0),
            single_adjustment_layer: Some(true),
        }));
        let unregistered = PremiereContext { adjustment_layer_registered: Some(false), ..video.clone() };
        let old = PremiereContext { host_version: Some("24.6".into()), ..video.clone() };
        let no_sequence = PremiereContext { sequence_id: None, ..video.clone() };
        let no_project = PremiereContext::default();

        assert_eq!(reason(Action::AddAdjustmentLayer, &video), None);
        assert_eq!(reason(Action::AddAdjustmentLayer, &none), Some(NO_VIDEO));
        assert_eq!(reason(Action::AddAdjustmentLayer, &audio), Some(NO_VIDEO));
        assert_eq!(reason(Action::AddAdjustmentLayer, &unregistered), Some(NOT_REGISTERED));
        assert_eq!(reason(Action::AddAdjustmentLayer, &old), Some(NEEDS_25_6));
        assert_eq!(reason(Action::AddAdjustmentLayer, &no_sequence), Some(NO_SEQUENCE));

        assert_eq!(reason(Action::RegisterAdjustmentLayer, &layer), None);
        assert_eq!(reason(Action::RegisterAdjustmentLayer, &video), Some(NOT_ADJUSTMENT_LAYER));
        assert_eq!(reason(Action::RegisterAdjustmentLayer, &no_project), Some(NO_PROJECT));

        assert_eq!(reason(Action::RemoveAttributes, &audio), None);
        assert_eq!(reason(Action::RemoveAttributes, &none), Some(NO_SELECTION));
        assert_eq!(reason(Action::RemoveAttributes, &old), Some(NEEDS_25_6));

        assert_eq!(reason(Action::AddMarker, &none), None);
        assert_eq!(reason(Action::AddMarker, &no_sequence), Some(NO_SEQUENCE));
        assert_eq!(reason(Action::GoToMarker, &no_sequence), None);
        assert_eq!(reason(Action::GoToMarker, &no_project), Some(NO_PROJECT));
        assert_eq!(reason(Action::LockHorizontalAxis, &no_project), None);
    }

    #[test]
    fn a_selection_without_a_sequence_is_ignored() {
        let stale = PremiereContext { sequence_id: None, ..context(selected(0, 0, 0)) };
        assert_eq!(
            primary(&effect_result("videoEffect"), &facts(&stale)),
            Availability::Blocked(NO_SEQUENCE)
        );
        assert_eq!(facts(&stale).selected, None);
    }

    #[test]
    fn versions_compare_by_major_and_minor() {
        for (version, expected) in [
            ("25.6", Some(true)),
            ("25.6.1", Some(true)),
            ("25.10.0", Some(true)),
            ("26.0.0", Some(true)),
            ("25.5.9", Some(false)),
            ("25", Some(false)),
            ("24.6.0", Some(false)),
            ("", None),
            ("beta", None),
        ] {
            assert_eq!(supports_25_6(version), expected, "{version}");
        }
    }

    #[test]
    fn summary_describes_the_sequence_and_selection() {
        assert_eq!(summary(None), "");
        assert_eq!(summary(Some(&PremiereContext::default())), "No project");
        assert_eq!(summary(Some(&PremiereContext { sequence_id: None, ..context(None) })), "No sequence");
        assert_eq!(summary(Some(&context(None))), "Sequence 01");
        assert_eq!(summary(Some(&context(selected(0, 0, 0)))), "Sequence 01 · No selection");
        assert_eq!(summary(Some(&context(selected(3, 2, 1)))), "Sequence 01 · 2 video, 1 audio");
        assert_eq!(summary(Some(&context(selected(1, 0, 1)))), "Sequence 01 · 1 audio");
        assert_eq!(
            summary(Some(&context(Some(SelectionSummary { total: 4, ..SelectionSummary::default() })))),
            "Sequence 01 · 4 selected"
        );
        let long = PremiereContext {
            sequence_name: Some("A very long sequence name".into()),
            ..context(None)
        };
        assert_eq!(summary(Some(&long)), "A very long seque…");
        let unnamed = PremiereContext { sequence_name: Some("  ".into()), ..context(None) };
        assert_eq!(summary(Some(&unnamed)), "Sequence");
    }
}
