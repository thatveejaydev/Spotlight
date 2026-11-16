//! Reads Premiere Pro's own settings: the newest preferences file, default
//! transition durations, and keyboard shortcut bindings from `.kys` profiles.
//! Results are cached and refreshed when the files change.

use serde::Serialize;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionDurationPref {
    pub value: f64,
    pub unit: String,
}

const SHORTCUTS_UNREADABLE: &str = "Could not read Premiere's keyboard shortcuts.";

/// Trimmed text of the first element named `tag` at or below `node`.
fn xml_text<'a>(node: roxmltree::Node<'a, '_>, tag: &str) -> Option<&'a str> {
    node.descendants()
        .find(|child| child.has_tag_name(tag))
        .and_then(|child| child.text())
        .map(str::trim)
}

/// A key chord: a Windows virtual-key code plus modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortcutSpec {
    pub vk: u16,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// How a Premiere command is bound in the active keyboard shortcut profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutBinding {
    Bound(ShortcutSpec),
    /// The profile lists the command with no key.
    Unbound,
}

/// Premiere's default Effect Controls binding (Shift+5), used when no profile can be read.
fn default_effect_controls_binding() -> ShortcutBinding {
    ShortcutBinding::Bound(default_effect_controls_shortcut())
}

pub fn default_effect_controls_shortcut() -> ShortcutSpec {
    ShortcutSpec {
        vk: b'5' as u16,
        ctrl: false,
        alt: false,
        shift: true,
    }
}

pub(crate) fn find_latest_premiere_prefs_file() -> Option<PathBuf> {
    let user_profile = env::var("USERPROFILE").ok()?;
    let candidates = [
        Path::new(&user_profile).join("Documents\\Adobe\\Premiere Pro"),
        Path::new(&user_profile).join("OneDrive\\Documents\\Adobe\\Premiere Pro"),
    ];

    let mut latest_file: Option<(PathBuf, SystemTime)> = None;

    for base in &candidates {
        let Ok(ver_dirs) = fs::read_dir(base) else {
            continue;
        };
        for ver_entry in ver_dirs.flatten() {
            let ver_path = ver_entry.path();
            if !ver_path.is_dir() {
                continue;
            }
            let Ok(prof_dirs) = fs::read_dir(&ver_path) else {
                continue;
            };
            for prof_entry in prof_dirs.flatten() {
                let prof_path = prof_entry.path();
                if !prof_path.is_dir() {
                    continue;
                }
                let prefs_file = prof_path.join("Adobe Premiere Pro Prefs");
                if let Ok(meta) = fs::metadata(&prefs_file) {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if latest_file.as_ref().map_or(true, |(_, t)| mtime > *t) {
                        latest_file = Some((prefs_file, mtime));
                    }
                }
            }
        }
    }

    latest_file.map(|(p, _)| p)
}

struct CachedTransitionPrefs {
    path: PathBuf,
    mtime: SystemTime,
    scanned_at: Instant,
    prefs: (TransitionDurationPref, TransitionDurationPref),
}

pub fn default_transition_prefs() -> (TransitionDurationPref, TransitionDurationPref) {
    (
        TransitionDurationPref {
            value: 30.0,
            unit: "frames".to_string(),
        },
        TransitionDurationPref {
            value: 1.0,
            unit: "seconds".to_string(),
        },
    )
}

fn parse_transition_prefs_xml(xml: &str) -> (TransitionDurationPref, TransitionDurationPref) {
    let (mut video_pref, mut audio_pref) = default_transition_prefs();
    let Ok(document) = roxmltree::Document::parse(xml) else {
        return (video_pref, audio_pref);
    };
    let root = document.root();

    // Video Transition Default Duration:
    // BE.Prefs.DefaultVideoTransitionDuration is stored in frames when unit == 0 (or seconds if DefaultVideoTransitionDurationInSeconds is present)
    let v_unit_code = xml_text(root, "BE.Prefs.DefaultVideoTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if v_unit_code == 1 {
        if let Some(sec) = xml_text(root, "BE.Prefs.DefaultVideoTransitionDurationInSeconds")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            video_pref = TransitionDurationPref {
                value: sec,
                unit: "seconds".to_string(),
            };
        } else if let Some(val) = xml_text(root, "BE.Prefs.DefaultVideoTransitionDuration")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            // Premiere stores DefaultVideoTransitionDuration as frames even when the UI displays Seconds
            video_pref = TransitionDurationPref {
                value: val,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(frames) = xml_text(root, "BE.Prefs.DefaultVideoTransitionDuration")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
    {
        video_pref = TransitionDurationPref {
            value: frames,
            unit: "frames".to_string(),
        };
    }

    // Audio Transition Default Duration:
    // Unit == 0 -> BE.Prefs.DefaultAudioTransitionDurationInFrames
    // Unit == 1 -> BE.Prefs.DefaultAudioTransitionDurationInSeconds
    let a_unit_code = xml_text(root, "BE.Prefs.DefaultAudioTransitionDurationUnit")
        .and_then(|s| s.parse::<i32>().ok())
        .unwrap_or(0);
    if a_unit_code == 0 {
        if let Some(frames) = xml_text(root, "BE.Prefs.DefaultAudioTransitionDurationInFrames")
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|v| *v > 0.0)
        {
            audio_pref = TransitionDurationPref {
                value: frames,
                unit: "frames".to_string(),
            };
        }
    } else if let Some(sec) = xml_text(root, "BE.Prefs.DefaultAudioTransitionDurationInSeconds")
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
    {
        audio_pref = TransitionDurationPref {
            value: sec,
            unit: "seconds".to_string(),
        };
    }

    (video_pref, audio_pref)
}

pub fn read_premiere_transition_prefs() -> (TransitionDurationPref, TransitionDurationPref) {
    const DIR_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
    static CACHE: OnceLock<Mutex<Option<CachedTransitionPrefs>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.scanned_at.elapsed() < DIR_RESCAN_INTERVAL {
                if let Ok(meta) = fs::metadata(&cached.path) {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    if mtime == cached.mtime {
                        return cached.prefs.clone();
                    }
                }
            }
        }
    }

    let Some(prefs_path) = find_latest_premiere_prefs_file() else {
        return default_transition_prefs();
    };
    let mtime = fs::metadata(&prefs_path)
        .and_then(|meta| meta.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if let Ok(mut guard) = cache.lock() {
        if let Some(cached) = guard.as_mut() {
            if cached.path == prefs_path && cached.mtime == mtime {
                cached.scanned_at = Instant::now();
                return cached.prefs.clone();
            }
        }
    }

    let Ok(xml) = fs::read_to_string(&prefs_path) else {
        return default_transition_prefs();
    };
    let prefs = parse_transition_prefs_xml(&xml);
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedTransitionPrefs {
            path: prefs_path,
            mtime,
            scanned_at: Instant::now(),
            prefs: prefs.clone(),
        });
    }
    prefs
}

fn decode_premiere_virtual_key(raw: u32) -> Option<u16> {
    if raw >= 0x8000_0000 {
        let vk = (raw & 0xFFFF) as u16;
        return (vk != 0).then_some(vk);
    }
    // Common Premiere internal DVA keycodes observed in .kys files.
    match raw {
        2 => Some(0x08),  // VK_BACK
        36 => Some(0x23), // VK_END
        37 => Some(0x24), // VK_HOME
        38 => Some(0x21), // VK_PRIOR (Page Up)
        39 => Some(0x22), // VK_NEXT (Page Down)
        42 => Some(0x25), // VK_LEFT
        43 => Some(0x27), // VK_RIGHT
        44 => Some(0x26), // VK_UP
        45 => Some(0x28), // VK_DOWN
        _ => None,
    }
}

fn parse_effect_controls_shortcut_kys(xml: &str) -> Option<ShortcutBinding> {
    let document = roxmltree::Document::parse(xml).ok()?;
    parse_shortcut_kys(&document, "context.global", "uif.window.Effect Controls")
}

/// Finds `command` among the `<item.N>` bindings inside `<context>` of a
/// Premiere `.kys` file. `context.global` falls back to the whole file when
/// the element is missing. Prefers a Shift-only binding when there are several.
///
/// Premiere 27 profiles split shortcuts into workspace modes, with
/// `<mode.Color>` before `<mode.Edit>`. Editing happens in Edit mode, so its
/// bindings win when the file has modes.
fn parse_shortcut_kys(
    document: &roxmltree::Document,
    context: &str,
    command: &str,
) -> Option<ShortcutBinding> {
    let root = document
        .descendants()
        .find(|node| node.has_tag_name("mode.Edit"))
        .unwrap_or_else(|| document.root());
    let scope = match root.descendants().find(|node| node.has_tag_name(context)) {
        Some(node) => node,
        None if context == "context.global" => root,
        None => return None,
    };

    let mut saw_command = false;
    let mut bindings = Vec::new();
    let items = scope
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name().starts_with("item."));

    for item in items {
        if xml_text(item, "commandname") != Some(command) {
            continue;
        }
        saw_command = true;
        let Some(raw_vk) = xml_text(item, "virtualkey").and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Some(vk) = decode_premiere_virtual_key(raw_vk) else {
            continue;
        };
        let flag = |tag| xml_text(item, tag) == Some("true");
        let (ctrl, alt, shift) = (flag("modifier.ctrl"), flag("modifier.alt"), flag("modifier.shift"));
        bindings.push(ShortcutSpec {
            vk,
            ctrl,
            alt,
            shift,
        });
    }

    if let Some(preferred) = bindings
        .iter()
        .copied()
        .find(|b| b.shift && !b.ctrl && !b.alt)
        .or_else(|| bindings.first().copied())
    {
        return Some(ShortcutBinding::Bound(preferred));
    }

    saw_command.then_some(ShortcutBinding::Unbound)
}

fn parse_preset_search_shortcuts(xml: &str) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    use ShortcutBinding::Bound;
    let doc = roxmltree::Document::parse(xml).map_err(|_| SHORTCUTS_UNREADABLE)?;

    let window = parse_shortcut_kys(&doc, "context.global", "uif.window.Effects");
    let Some(Bound(effects)) = window
        .filter(|binding| matches!(binding, Bound(_)))
        .or_else(|| parse_shortcut_kys(&doc, "context.global", "cmd.window.effects"))
    else {
        return Err("Assign a shortcut to Effects in Premiere Keyboard Shortcuts.");
    };
    let find_box = parse_shortcut_kys(&doc, "context.effects", "cmd.select.find.box")
        .or_else(|| parse_shortcut_kys(&doc, "context.global", "cmd.select.find.box"));
    let Some(Bound(find_box)) = find_box else {
        return Err("Assign a shortcut to Select Find Box in Premiere Keyboard Shortcuts.");
    };
    Ok((effects, find_box))
}

pub(crate) fn read_preset_search_shortcuts(
    premiere_exe: &Path,
) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    parse_preset_search_shortcuts(&read_premiere_shortcut_profile(premiere_exe)?)
}

pub(crate) fn read_marker_shortcuts(
    premiere_exe: &Path,
) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    parse_marker_shortcuts(&read_premiere_shortcut_profile(premiere_exe)?)
}

fn parse_marker_shortcuts(xml: &str) -> Result<(ShortcutSpec, ShortcutSpec), &'static str> {
    use ShortcutBinding::Bound;
    let doc = roxmltree::Document::parse(xml).map_err(|_| SHORTCUTS_UNREADABLE)?;
    let Some(Bound(timeline)) = parse_shortcut_kys(&doc, "context.global", "uif.window.Timelines") else {
        return Err("Assign a shortcut to Timeline in Premiere Keyboard Shortcuts.");
    };
    let add_marker = parse_shortcut_kys(&doc, "context.timeline", "cmd.set.marker")
        .or_else(|| parse_shortcut_kys(&doc, "context.global", "cmd.set.marker"));
    let Some(Bound(add_marker)) = add_marker else {
        return Err("Assign a shortcut to Add Marker in Premiere Keyboard Shortcuts.");
    };
    Ok((timeline, add_marker))
}

fn read_premiere_shortcut_profile(premiere_exe: &Path) -> Result<String, &'static str> {
    const UNREADABLE: &str = SHORTCUTS_UNREADABLE;
    let mut name = None;
    let mut custom_path = None;
    if let Some(prefs) = find_latest_premiere_prefs_file() {
        let xml = fs::read_to_string(&prefs).map_err(|_| UNREADABLE)?;
        let document = roxmltree::Document::parse(&xml).map_err(|_| UNREADABLE)?;
        name = document
            .descendants()
            .find(|node| node.has_tag_name("FE.Prefs.Shortcuts.Filename"))
            .and_then(|node| node.text())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if let (Some(name), Some(profile)) = (&name, prefs.parent()) {
            if Path::new(name).file_name().is_none_or(|file| file != name.as_str()) {
                return Err(UNREADABLE);
            }
            custom_path = Some(profile.join("Win").join(name));
        }
    }

    let xml = match custom_path.map(fs::read_to_string) {
        Some(Ok(xml)) => xml,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => return Err(UNREADABLE),
        _ => {
            // Premiere stores its bundled shortcut sets beside the executable.
            // The command identifiers in those sets are language independent.
            let bundled = premiere_exe
                .parent()
                .ok_or(UNREADABLE)?
                .join("Keyboard Shortcuts")
                .join("en");
            let name = name.as_deref().unwrap_or("Adobe Premiere Pro Defaults.kys");
            fs::read_to_string(bundled.join(name)).map_err(|_| UNREADABLE)?
        }
    };
    Ok(xml)
}

struct CachedShortcutBinding {
    prefs_path: PathBuf,
    prefs_mtime: SystemTime,
    kys_path: Option<PathBuf>,
    kys_mtime: SystemTime,
    scanned_at: Instant,
    shortcut: ShortcutBinding,
}

pub fn read_effect_controls_shortcut() -> ShortcutBinding {
    const DIR_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
    static CACHE: OnceLock<Mutex<Option<CachedShortcutBinding>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));

    let file_mtime = |path: &Path| {
        fs::metadata(path)
            .and_then(|meta| meta.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH)
    };

    if let Ok(guard) = cache.lock() {
        if let Some(cached) = guard.as_ref() {
            if cached.scanned_at.elapsed() < DIR_RESCAN_INTERVAL
                && cached.prefs_path.exists()
                && file_mtime(&cached.prefs_path) == cached.prefs_mtime
            {
                let kys_unchanged = match &cached.kys_path {
                    Some(kys) => kys.exists() && file_mtime(kys) == cached.kys_mtime,
                    None => true,
                };
                if kys_unchanged {
                    return cached.shortcut;
                }
            }
        }
    }

    let Some(prefs_path) = find_latest_premiere_prefs_file() else {
        return default_effect_controls_binding();
    };
    let prefs_mtime = file_mtime(&prefs_path);
    let Ok(prefs_xml) = fs::read_to_string(&prefs_path) else {
        return default_effect_controls_binding();
    };

    let kys_path = roxmltree::Document::parse(&prefs_xml)
        .ok()
        .and_then(|doc| xml_text(doc.root(), "FE.Prefs.Shortcuts.Filename").map(str::to_owned))
        .filter(|name| !name.is_empty())
        // A bare file name only, so the prefs file cannot point outside "Win".
        .filter(|name| Path::new(name).file_name().is_some_and(|file| file == name.as_str()))
        .and_then(|name| prefs_path.parent().map(|dir| dir.join("Win").join(name)))
        .filter(|path| path.is_file());

    let kys_mtime = kys_path
        .as_deref()
        .map(file_mtime)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    if let Ok(mut guard) = cache.lock() {
        if let Some(cached) = guard.as_mut() {
            if cached.prefs_path == prefs_path
                && cached.prefs_mtime == prefs_mtime
                && cached.kys_path == kys_path
                && cached.kys_mtime == kys_mtime
            {
                cached.scanned_at = Instant::now();
                return cached.shortcut;
            }
        }
    }

    let shortcut = kys_path
        .as_deref()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|xml| parse_effect_controls_shortcut_kys(&xml))
        .unwrap_or_else(default_effect_controls_binding);

    if let Ok(mut guard) = cache.lock() {
        *guard = Some(CachedShortcutBinding {
            prefs_path,
            prefs_mtime,
            kys_path,
            kys_mtime,
            scanned_at: Instant::now(),
            shortcut,
        });
    }
    shortcut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_shortcuts_respect_the_timeline_override_and_unbound_commands() {
        let profile = r#"<context.global>
          <item.1><commandname>uif.window.Timelines</commandname><modifier.shift>true</modifier.shift><virtualkey>2147483699</virtualkey></item.1>
          <item.2><commandname>cmd.set.marker</commandname><virtualkey>2147483725</virtualkey></item.2>
        </context.global>"#;
        let (timeline, add) = parse_marker_shortcuts(profile).unwrap();
        assert_eq!(timeline.vk, u16::from(b'3'));
        assert!(timeline.shift);
        assert_eq!(add.vk, u16::from(b'M'));

        let custom = format!(r#"<PremiereData>{profile}<context.timeline>
          <item.3><commandname>cmd.set.marker</commandname><modifier.alt>true</modifier.alt><virtualkey>2147483726</virtualkey></item.3>
        </context.timeline></PremiereData>"#);
        let (_, add) = parse_marker_shortcuts(&custom).unwrap();
        assert_eq!(add.vk, u16::from(b'N'));
        assert!(add.alt);
        assert!(parse_marker_shortcuts(&custom.replace("<virtualkey>2147483726</virtualkey>", "")).is_err());
    }

    #[test]
    fn parses_effect_controls_shortcut_from_kys() {
        // Prefers Shift-only binding when multiple bindings exist (like [Custom].kys).
        let multi = r#"
        <PremiereData Version="3"><shortcuts Version="5">
          <context.global Version="1">
            <item.881 Version="1">
              <virtualkey>2147483698</virtualkey>
              <modifier.ctrl>false</modifier.ctrl>
              <modifier.alt>false</modifier.alt>
              <modifier.shift>false</modifier.shift>
              <commandname>uif.window.Effect Controls</commandname>
            </item.881>
            <item.880 Version="1">
              <virtualkey>2147483701</virtualkey>
              <modifier.ctrl>false</modifier.ctrl>
              <modifier.alt>false</modifier.alt>
              <modifier.shift>true</modifier.shift>
              <commandname>uif.window.Effect Controls</commandname>
            </item.880>
          </context.global>
        </shortcuts></PremiereData>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(multi),
            Some(ShortcutBinding::Bound(ShortcutSpec {
                vk: b'5' as u16,
                ctrl: false,
                alt: false,
                shift: true,
            }))
        );

        // Uses single custom binding when Shift+5 is remapped.
        let custom = r#"
        <context.global Version="1">
          <item.10 Version="1">
            <virtualkey>2147483717</virtualkey>
            <modifier.ctrl>true</modifier.ctrl>
            <modifier.alt>false</modifier.alt>
            <modifier.shift>false</modifier.shift>
            <commandname>uif.window.Effect Controls</commandname>
          </item.10>
        </context.global>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(custom),
            Some(ShortcutBinding::Bound(ShortcutSpec {
                vk: b'E' as u16,
                ctrl: true,
                alt: false,
                shift: false,
            }))
        );

        // Recognizes explicitly unbound command.
        let unbound = r#"
        <context.global Version="1">
          <item.880 Version="1">
            <commandname>uif.window.Effect Controls</commandname>
          </item.880>
        </context.global>"#;
        assert_eq!(
            parse_effect_controls_shortcut_kys(unbound),
            Some(ShortcutBinding::Unbound)
        );

        // A missing entry returns None; the caller then uses Premiere's default binding.
        let missing = r#"<context.global Version="1"></context.global>"#;
        assert_eq!(parse_effect_controls_shortcut_kys(missing), None);
    }

    #[test]
    fn shortcut_parser_handles_xml_the_old_string_search_missed() {
        // A comment that mentions the command, a self-closing modifier tag,
        // an escaped character, and attributes on every element.
        let kys = r#"<?xml version="1.0" encoding="UTF-8"?>
        <PremiereData Version="3">
          <!-- <item.1><commandname>uif.window.Timelines</commandname></item.1> -->
          <context.global Version="1">
            <item.5 Version="1"><commandname>cmd.a&amp;b</commandname><virtualkey>2147483713</virtualkey></item.5>
            <item.7 Version="1">
              <commandname Type="s">uif.window.Timelines</commandname>
              <modifier.ctrl/>
              <modifier.shift Type="b">true</modifier.shift>
              <virtualkey Type="u">2147483699</virtualkey>
            </item.7>
          </context.global>
        </PremiereData>"#;
        let doc = roxmltree::Document::parse(kys).unwrap();
        assert_eq!(
            parse_shortcut_kys(&doc, "context.global", "uif.window.Timelines"),
            Some(ShortcutBinding::Bound(ShortcutSpec {
                vk: u16::from(b'3'),
                ctrl: false,
                alt: false,
                shift: true,
            }))
        );
        assert!(matches!(
            parse_shortcut_kys(&doc, "context.global", "cmd.a&b"),
            Some(ShortcutBinding::Bound(ShortcutSpec { vk, .. })) if vk == u16::from(b'A')
        ));
        assert_eq!(parse_marker_shortcuts("<context.global>").unwrap_err(), SHORTCUTS_UNREADABLE);
    }

    #[test]
    fn premiere_27_profiles_use_edit_mode_bindings() {
        // Color mode comes first in the file and leaves Find Box unbound.
        let kys = r#"<PremiereData Version="3"><shortcuts Version="6">
          <mode.Color Version="1">
            <context.global Version="1">
              <item.1><commandname>cmd.select.find.box</commandname></item.1>
            </context.global>
          </mode.Color>
          <mode.Edit Version="1">
            <context.global Version="1">
              <item.2><commandname>cmd.select.find.box</commandname><modifier.shift>true</modifier.shift><virtualkey>2147483718</virtualkey></item.2>
            </context.global>
          </mode.Edit>
        </shortcuts></PremiereData>"#;
        let doc = roxmltree::Document::parse(kys).unwrap();
        assert!(matches!(
            parse_shortcut_kys(&doc, "context.global", "cmd.select.find.box"),
            Some(ShortcutBinding::Bound(ShortcutSpec { vk, shift: true, .. })) if vk == u16::from(b'F')
        ));
    }

    #[test]
    fn reads_transition_prefs_with_the_xml_parser() {
        let prefs = r#"<PremiereData>
          <BE.Prefs.DefaultVideoTransitionDurationUnit Type="i">1</BE.Prefs.DefaultVideoTransitionDurationUnit>
          <BE.Prefs.DefaultVideoTransitionDurationInSeconds Type="f">1.5</BE.Prefs.DefaultVideoTransitionDurationInSeconds>
          <BE.Prefs.DefaultAudioTransitionDurationUnit>0</BE.Prefs.DefaultAudioTransitionDurationUnit>
          <BE.Prefs.DefaultAudioTransitionDurationInFrames>12</BE.Prefs.DefaultAudioTransitionDurationInFrames>
        </PremiereData>"#;
        let (video, audio) = parse_transition_prefs_xml(prefs);
        assert_eq!((video.value, video.unit.as_str()), (1.5, "seconds"));
        assert_eq!((audio.value, audio.unit.as_str()), (12.0, "frames"));

        let (video, audio) = parse_transition_prefs_xml("<broken");
        let (default_video, default_audio) = default_transition_prefs();
        assert_eq!((video.value, audio.value), (default_video.value, default_audio.value));
    }

    #[test]
    fn uses_premiere_bindings_for_effects_and_the_find_box() {
        let shortcuts = r#"<PremiereData Version="3">
        <context.global Version="1">
          <item.10 Version="1">
            <commandname>uif.window.Effects</commandname>
            <virtualkey>2147483717</virtualkey>
            <modifier.ctrl>true</modifier.ctrl>
          </item.10>
          <item.11 Version="1">
            <commandname>cmd.select.find.box</commandname>
            <virtualkey>2147483718</virtualkey>
            <modifier.shift>true</modifier.shift>
          </item.11>
        </context.global>
        <context.effects Version="1">
          <item.0 Version="1">
            <commandname>cmd.select.find.box</commandname>
            <virtualkey>2147483731</virtualkey>
            <modifier.alt>true</modifier.alt>
          </item.0>
        </context.effects></PremiereData>"#;
        let (effects, find_box) = parse_preset_search_shortcuts(shortcuts).unwrap();
        assert_eq!(
            effects,
            ShortcutSpec {
                vk: b'E' as u16,
                ctrl: true,
                alt: false,
                shift: false,
            }
        );
        assert_eq!(
            find_box,
            ShortcutSpec {
                vk: b'S' as u16,
                ctrl: false,
                alt: true,
                shift: false,
            }
        );
        let unbound = shortcuts.replace("<virtualkey>2147483731</virtualkey>", "");
        assert!(parse_preset_search_shortcuts(&unbound)
            .unwrap_err()
            .contains("Select Find Box"));
        let unbound = shortcuts.replace("<virtualkey>2147483717</virtualkey>", "");
        assert!(parse_preset_search_shortcuts(&unbound)
            .unwrap_err()
            .contains("Effects"));
    }
}
