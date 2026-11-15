use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use serde::{Deserialize, Serialize};
use slint::platform::Key;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

pub const DEFAULT_HOTKEY: &str = "Ctrl+Space";
pub const DEFAULT_SETTINGS_HOTKEY: &str = "Ctrl+,";
pub const DEFAULT_AUDIO_PREVIEW_HOTKEY: &str = "Ctrl+P";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AlternateModifier {
    #[default]
    Shift,
    Ctrl,
    Alt,
}

impl AlternateModifier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shift => "shift",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
        }
    }

    pub fn from_setting(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Self::Ctrl,
            "alt" | "option" => Self::Alt,
            _ => Self::Shift,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub hotkey: String,
    pub open_effect_controls_by_default: bool,
    pub alternate_modifier: AlternateModifier,
    pub settings_hotkey: String,
    pub catalog_all_hotkey: String,
    pub catalog_effects_hotkey: String,
    pub catalog_project_hotkey: String,
    pub audio_preview_hotkey: String,
    pub auto_preview_audio: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_string(),
            open_effect_controls_by_default: false,
            alternate_modifier: AlternateModifier::Shift,
            settings_hotkey: DEFAULT_SETTINGS_HOTKEY.to_string(),
            catalog_all_hotkey: String::new(),
            catalog_effects_hotkey: String::new(),
            catalog_project_hotkey: String::new(),
            audio_preview_hotkey: DEFAULT_AUDIO_PREVIEW_HOTKEY.into(),
            auto_preview_audio: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteShortcutAction {
    ToggleSettings,
    SelectCatalog(&'static str),
    PreviewAudio,
}

pub fn config_path() -> PathBuf {
    if let Ok(appdata) = env::var("APPDATA") {
        let trimmed = appdata.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed)
                .join("PremiereSpotlight")
                .join("config.json");
        }
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("premiere-spotlight-config.json");
        }
    }
    PathBuf::from("premiere-spotlight-config.json")
}

pub fn load_settings() -> AppSettings {
    normalize_settings(load_settings_from(&config_path()))
}

fn normalize_settings(mut settings: AppSettings) -> AppSettings {
    settings.hotkey = parse_hotkey_spec(&settings.hotkey)
        .map(|(_, canonical)| canonical)
        .unwrap_or_else(|_| DEFAULT_HOTKEY.to_string());
    settings.settings_hotkey = normalize_optional_hotkey(
        &settings.settings_hotkey,
        DEFAULT_SETTINGS_HOTKEY,
    );
    settings.catalog_all_hotkey = normalize_optional_hotkey(&settings.catalog_all_hotkey, "");
    settings.catalog_effects_hotkey =
        normalize_optional_hotkey(&settings.catalog_effects_hotkey, "");
    settings.catalog_project_hotkey =
        normalize_optional_hotkey(&settings.catalog_project_hotkey, "");
    settings.audio_preview_hotkey =
        normalize_optional_hotkey(&settings.audio_preview_hotkey, DEFAULT_AUDIO_PREVIEW_HOTKEY);
    if shortcut_conflict("audio_preview", &settings.audio_preview_hotkey, &settings).is_some() {
        settings.audio_preview_hotkey.clear();
    }
    settings
}

fn normalize_optional_hotkey(spec: &str, fallback: &str) -> String {
    if spec.trim().is_empty() {
        return String::new();
    }
    parse_palette_hotkey_spec(spec)
        .map(|(_, canonical)| canonical)
        .unwrap_or_else(|_| fallback.to_string())
}

pub fn load_settings_from(path: &Path) -> AppSettings {
    let Ok(raw) = fs::read_to_string(path) else {
        return AppSettings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_settings(settings: &AppSettings) -> Result<(), String> {
    save_settings_to(&config_path(), settings)
}

pub fn save_settings_to(path: &Path, settings: &AppSettings) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create settings folder: {e}"))?;
        }
    }
    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {e}"))?;
    fs::write(path, json).map_err(|e| format!("Failed to save settings: {e}"))
}

pub fn resolve_open_effect_controls(open_by_default: bool, is_shift_activate: bool) -> bool {
    open_by_default ^ is_shift_activate
}

pub fn parse_hotkey_spec(spec: &str) -> Result<(HotKey, String), String> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err("Hotkey cannot be empty.".into());
    }
    let hotkey: HotKey = trimmed
        .parse()
        .map_err(|_| format!("Unrecognized hotkey \"{trimmed}\". Try e.g. Ctrl+Space or Alt+Space."))?;
    if !is_valid_hotkey(hotkey.mods, hotkey.key) {
        return Err("Include Ctrl, Alt, or Shift (or use a Function key F1–F24).".into());
    }
    let label = format_hotkey(hotkey.mods, hotkey.key);
    Ok((hotkey, label))
}

pub fn parse_palette_hotkey_spec(spec: &str) -> Result<(HotKey, String), String> {
    let (hotkey, label) = parse_hotkey_spec(spec)?;
    if !is_valid_palette_hotkey(hotkey.mods, hotkey.key) {
        return Err("In-palette shortcuts must use Ctrl, Alt, or F1–F24.".into());
    }
    Ok((hotkey, label))
}

fn is_valid_hotkey(mods: Modifiers, code: Code) -> bool {
    let has_modifier = mods.intersects(
        Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT | Modifiers::SUPER,
    );
    has_modifier || is_function_key(code)
}

fn is_valid_palette_hotkey(mods: Modifiers, code: Code) -> bool {
    let has_safe_modifier = mods.intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER);
    has_safe_modifier || is_function_key(code)
}

fn is_function_key(code: Code) -> bool {
    matches!(
        code,
        Code::F1
            | Code::F2
            | Code::F3
            | Code::F4
            | Code::F5
            | Code::F6
            | Code::F7
            | Code::F8
            | Code::F9
            | Code::F10
            | Code::F11
            | Code::F12
            | Code::F13
            | Code::F14
            | Code::F15
            | Code::F16
            | Code::F17
            | Code::F18
            | Code::F19
            | Code::F20
            | Code::F21
            | Code::F22
            | Code::F23
            | Code::F24
    )
}

pub fn format_hotkey(mods: Modifiers, code: Code) -> String {
    let mut parts: Vec<String> = Vec::new();
    if mods.contains(Modifiers::CONTROL) {
        parts.push("Ctrl".into());
    }
    if mods.contains(Modifiers::ALT) {
        parts.push("Alt".into());
    }
    if mods.contains(Modifiers::SHIFT) {
        parts.push("Shift".into());
    }
    if mods.intersects(Modifiers::SUPER | Modifiers::META) {
        parts.push("Win".into());
    }
    parts.push(format_code(code));
    parts.join("+")
}

fn format_code(code: Code) -> String {
    let raw = code.to_string();
    if let Some(letter) = raw.strip_prefix("Key") {
        return letter.to_string();
    }
    if let Some(digit) = raw.strip_prefix("Digit") {
        return digit.to_string();
    }
    match code {
        Code::Backquote => "`".into(),
        Code::Minus => "-".into(),
        Code::Equal => "=".into(),
        Code::BracketLeft => "[".into(),
        Code::BracketRight => "]".into(),
        Code::Backslash => "\\".into(),
        Code::Semicolon => ";".into(),
        Code::Quote => "'".into(),
        Code::Comma => ",".into(),
        Code::Period => ".".into(),
        Code::Slash => "/".into(),
        _ => raw,
    }
}

pub fn hotkey_from_slint_event(
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<Option<(HotKey, String)>, String> {
    capture_slint_event(text, ctrl, alt, shift, meta, false)
}

pub fn palette_hotkey_from_slint_event(
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Result<Option<(HotKey, String)>, String> {
    capture_slint_event(text, ctrl, alt, shift, meta, true)
}

fn capture_slint_event(
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
    require_palette_safe: bool,
) -> Result<Option<(HotKey, String)>, String> {
    let Some(ch) = text.chars().next() else {
        return Ok(None);
    };
    // Ignore lone modifier or lock key presses while the user is building a chord.
    if is_modifier_or_ignored_slint_char(ch) {
        return Ok(None);
    }

    let Some(code) = slint_char_to_code(ch) else {
        return Err("Unsupported key for shortcut.".into());
    };

    let mut mods = Modifiers::empty();
    if ctrl {
        mods |= Modifiers::CONTROL;
    }
    if alt {
        mods |= Modifiers::ALT;
    }
    if shift {
        mods |= Modifiers::SHIFT;
    }
    if meta {
        mods |= Modifiers::SUPER;
    }

    if require_palette_safe {
        if !is_valid_palette_hotkey(mods, code) {
            return Err("Hold Ctrl or Alt (or press F1–F24) so typing isn't intercepted.".into());
        }
    } else if !is_valid_hotkey(mods, code) {
        return Err("Hold Ctrl, Alt, or Shift (or press F1–F24).".into());
    }

    let hotkey = HotKey::new((!mods.is_empty()).then_some(mods), code);
    let label = format_hotkey(hotkey.mods, hotkey.key);
    Ok(Some((hotkey, label)))
}

pub fn match_palette_shortcut(
    settings: &AppSettings,
    settings_open: bool,
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Option<PaletteShortcutAction> {
    let Ok(Some((pressed, _))) = palette_hotkey_from_slint_event(text, ctrl, alt, shift, meta)
    else {
        return None;
    };
    let matches_spec = |spec: &str| {
        !spec.trim().is_empty()
            && parse_palette_hotkey_spec(spec)
                .ok()
                .is_some_and(|(hk, _)| hk.id() == pressed.id())
    };
    if matches_spec(&settings.settings_hotkey) {
        return Some(PaletteShortcutAction::ToggleSettings);
    }
    if settings_open {
        return None;
    }
    for (spec, catalog) in [
        (&settings.catalog_all_hotkey, "all"),
        (&settings.catalog_effects_hotkey, "effects"),
        (&settings.catalog_project_hotkey, "project"),
    ] {
        if matches_spec(spec) {
            return Some(PaletteShortcutAction::SelectCatalog(catalog));
        }
    }
    matches_spec(&settings.audio_preview_hotkey).then_some(PaletteShortcutAction::PreviewAudio)
}

pub fn shortcut_conflict(target: &str, label: &str, settings: &AppSettings) -> Option<String> {
    if target == "audio_preview"
        && parse_palette_hotkey_spec(label).is_ok_and(|(hotkey, _)| {
            matches!(
                hotkey.key,
                Code::Enter | Code::Tab | Code::ArrowUp | Code::ArrowDown | Code::Backspace | Code::F12
            )
        })
    {
        return Some("That key is reserved for palette navigation or actions.".into());
    }
    let slots = [
        ("launch", "Launch Hotkey", settings.hotkey.as_str()),
        ("settings", "Open Settings", settings.settings_hotkey.as_str()),
        ("catalog_all", "All Catalog", settings.catalog_all_hotkey.as_str()),
        (
            "catalog_effects",
            "Effects Catalog",
            settings.catalog_effects_hotkey.as_str(),
        ),
        (
            "catalog_project",
            "Project Catalog",
            settings.catalog_project_hotkey.as_str(),
        ),
        ("audio_preview", "Preview Audio", settings.audio_preview_hotkey.as_str()),
    ];
    for (slot, name, existing) in slots {
        if slot != target && !existing.is_empty() && existing.eq_ignore_ascii_case(label) {
            return Some(format!("{label} is already assigned to {name}."));
        }
    }
    None
}

fn is_modifier_or_ignored_slint_char(ch: char) -> bool {
    [
        Key::Shift,
        Key::ShiftR,
        Key::Control,
        Key::ControlR,
        Key::Alt,
        Key::AltGr,
        Key::Meta,
        Key::MetaR,
        Key::CapsLock,
        Key::Escape,
    ]
    .into_iter()
    .map(char::from)
    .any(|k| k == ch)
}

fn slint_char_to_code(ch: char) -> Option<Code> {
    if ch == char::from(Key::Space) {
        return Some(Code::Space);
    }
    if ch == char::from(Key::Return) {
        return Some(Code::Enter);
    }
    if ch == char::from(Key::Tab) || ch == char::from(Key::Backtab) {
        return Some(Code::Tab);
    }
    if ch == char::from(Key::Backspace) {
        return Some(Code::Backspace);
    }
    if ch == char::from(Key::Delete) {
        return Some(Code::Delete);
    }
    if ch == char::from(Key::Insert) {
        return Some(Code::Insert);
    }
    if ch == char::from(Key::Home) {
        return Some(Code::Home);
    }
    if ch == char::from(Key::End) {
        return Some(Code::End);
    }
    if ch == char::from(Key::PageUp) {
        return Some(Code::PageUp);
    }
    if ch == char::from(Key::PageDown) {
        return Some(Code::PageDown);
    }
    if ch == char::from(Key::UpArrow) {
        return Some(Code::ArrowUp);
    }
    if ch == char::from(Key::DownArrow) {
        return Some(Code::ArrowDown);
    }
    if ch == char::from(Key::LeftArrow) {
        return Some(Code::ArrowLeft);
    }
    if ch == char::from(Key::RightArrow) {
        return Some(Code::ArrowRight);
    }
    if ch == char::from(Key::Pause) {
        return Some(Code::Pause);
    }
    if ch == char::from(Key::ScrollLock) {
        return Some(Code::ScrollLock);
    }

    let fn_keys = [
        (Key::F1, Code::F1),
        (Key::F2, Code::F2),
        (Key::F3, Code::F3),
        (Key::F4, Code::F4),
        (Key::F5, Code::F5),
        (Key::F6, Code::F6),
        (Key::F7, Code::F7),
        (Key::F8, Code::F8),
        (Key::F9, Code::F9),
        (Key::F10, Code::F10),
        (Key::F11, Code::F11),
        (Key::F12, Code::F12),
        (Key::F13, Code::F13),
        (Key::F14, Code::F14),
        (Key::F15, Code::F15),
        (Key::F16, Code::F16),
        (Key::F17, Code::F17),
        (Key::F18, Code::F18),
        (Key::F19, Code::F19),
        (Key::F20, Code::F20),
        (Key::F21, Code::F21),
        (Key::F22, Code::F22),
        (Key::F23, Code::F23),
        (Key::F24, Code::F24),
    ];
    if let Some((_, code)) = fn_keys.into_iter().find(|(k, _)| char::from(*k) == ch) {
        return Some(code);
    }

    match ch.to_ascii_uppercase() {
        'A' => Some(Code::KeyA),
        'B' => Some(Code::KeyB),
        'C' => Some(Code::KeyC),
        'D' => Some(Code::KeyD),
        'E' => Some(Code::KeyE),
        'F' => Some(Code::KeyF),
        'G' => Some(Code::KeyG),
        'H' => Some(Code::KeyH),
        'I' => Some(Code::KeyI),
        'J' => Some(Code::KeyJ),
        'K' => Some(Code::KeyK),
        'L' => Some(Code::KeyL),
        'M' => Some(Code::KeyM),
        'N' => Some(Code::KeyN),
        'O' => Some(Code::KeyO),
        'P' => Some(Code::KeyP),
        'Q' => Some(Code::KeyQ),
        'R' => Some(Code::KeyR),
        'S' => Some(Code::KeyS),
        'T' => Some(Code::KeyT),
        'U' => Some(Code::KeyU),
        'V' => Some(Code::KeyV),
        'W' => Some(Code::KeyW),
        'X' => Some(Code::KeyX),
        'Y' => Some(Code::KeyY),
        'Z' => Some(Code::KeyZ),
        '0' | ')' => Some(Code::Digit0),
        '1' | '!' => Some(Code::Digit1),
        '2' | '@' => Some(Code::Digit2),
        '3' | '#' => Some(Code::Digit3),
        '4' | '$' => Some(Code::Digit4),
        '5' | '%' => Some(Code::Digit5),
        '6' | '^' => Some(Code::Digit6),
        '7' | '&' => Some(Code::Digit7),
        '8' | '*' => Some(Code::Digit8),
        '9' | '(' => Some(Code::Digit9),
        '`' | '~' => Some(Code::Backquote),
        '-' | '_' => Some(Code::Minus),
        '=' | '+' => Some(Code::Equal),
        '[' | '{' => Some(Code::BracketLeft),
        ']' | '}' => Some(Code::BracketRight),
        '\\' | '|' => Some(Code::Backslash),
        ';' | ':' => Some(Code::Semicolon),
        '\'' | '"' => Some(Code::Quote),
        ',' | '<' => Some(Code::Comma),
        '.' | '>' => Some(Code::Period),
        '/' | '?' => Some(Code::Slash),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn settings_round_trip_and_invalid_json_fallback() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = env::temp_dir().join(format!("premiere_spotlight_settings_{unique}"));
        let file = dir.join("config.json");

        assert_eq!(load_settings_from(&file), AppSettings::default());

        let saved = AppSettings {
            hotkey: "Alt+Space".into(),
            open_effect_controls_by_default: true,
            alternate_modifier: AlternateModifier::Ctrl,
            settings_hotkey: "Ctrl+,".into(),
            catalog_all_hotkey: "Ctrl+1".into(),
            catalog_effects_hotkey: "Ctrl+2".into(),
            catalog_project_hotkey: "Ctrl+3".into(),
            audio_preview_hotkey: "Alt+P".into(),
            auto_preview_audio: true,
        };
        save_settings_to(&file, &saved).unwrap();
        assert_eq!(load_settings_from(&file), saved);

        fs::write(&file, "{ not valid json").unwrap();
        assert_eq!(load_settings_from(&file), AppSettings::default());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn audio_preview_shortcut_defaults_and_preserves_existing_bindings() {
        let default = normalize_settings(serde_json::from_str("{}").unwrap());
        assert_eq!(default.audio_preview_hotkey, "Ctrl+P");
        assert!(!default.auto_preview_audio);
        assert_eq!(
            match_palette_shortcut(&default, false, "p", true, false, false, false),
            Some(PaletteShortcutAction::PreviewAudio)
        );
        assert_eq!(match_palette_shortcut(&default, true, "p", true, false, false, false), None);
        assert!(shortcut_conflict("audio_preview", "Alt+Enter", &default).is_some());
        assert!(shortcut_conflict("catalog_project", "Ctrl+P", &default).is_some());

        let migrated = normalize_settings(
            serde_json::from_str(r#"{"catalog_project_hotkey":"ctrl+p"}"#).unwrap(),
        );
        assert_eq!(migrated.catalog_project_hotkey, "Ctrl+P");
        assert!(migrated.audio_preview_hotkey.is_empty());
        let cleared = normalize_settings(
            serde_json::from_str(r#"{"audio_preview_hotkey":"","auto_preview_audio":true}"#).unwrap(),
        );
        assert!(cleared.audio_preview_hotkey.is_empty());
        assert!(cleared.auto_preview_audio);
    }

    #[test]
    fn matches_configurable_palette_shortcuts_and_detects_conflicts() {
        let mut settings = AppSettings::default();
        assert_eq!(settings.settings_hotkey, "Ctrl+,");
        assert!(settings.catalog_all_hotkey.is_empty());
        assert!(settings.catalog_effects_hotkey.is_empty());
        assert!(settings.catalog_project_hotkey.is_empty());

        // Shift+, should not match Ctrl+, nor hijack Shift typing.
        assert!(palette_hotkey_from_slint_event("A", false, false, true, false).is_err());
        assert_eq!(
            match_palette_shortcut(&settings, false, ",", true, false, false, false),
            Some(PaletteShortcutAction::ToggleSettings)
        );
        // Ctrl+, still toggles back when Settings is already open.
        assert_eq!(
            match_palette_shortcut(&settings, true, ",", true, false, false, false),
            Some(PaletteShortcutAction::ToggleSettings)
        );

        settings.catalog_all_hotkey = "Ctrl+1".into();
        settings.catalog_effects_hotkey = "Ctrl+2".into();
        settings.catalog_project_hotkey = "Ctrl+3".into();
        assert_eq!(
            match_palette_shortcut(&settings, false, "1", true, false, false, false),
            Some(PaletteShortcutAction::SelectCatalog("all"))
        );
        assert_eq!(
            match_palette_shortcut(&settings, false, "2", true, false, false, false),
            Some(PaletteShortcutAction::SelectCatalog("effects"))
        );
        assert_eq!(
            match_palette_shortcut(&settings, false, "3", true, false, false, false),
            Some(PaletteShortcutAction::SelectCatalog("project"))
        );
        // Catalog shortcuts stay inactive while Settings is open.
        assert_eq!(
            match_palette_shortcut(&settings, true, "1", true, false, false, false),
            None
        );

        assert!(shortcut_conflict("catalog_all", "Ctrl+,", &settings).is_some());
        assert!(shortcut_conflict("settings", "Ctrl+,", &settings).is_none());
    }

    #[test]
    fn parses_and_formats_hotkeys() {
        let (hk, label) = parse_hotkey_spec("control+shift+keyp").unwrap();
        assert_eq!(label, "Ctrl+Shift+P");
        assert_eq!(hk, HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyP));

        let (_, space_label) = parse_hotkey_spec("Alt+Space").unwrap();
        assert_eq!(space_label, "Alt+Space");

        let (_, f8_label) = parse_hotkey_spec("F8").unwrap();
        assert_eq!(f8_label, "F8");

        assert!(parse_hotkey_spec("").is_err());
        assert!(parse_hotkey_spec("Space").is_err());
        assert!(parse_hotkey_spec("Ctrl+NotAKey").is_err());
    }

    #[test]
    fn captures_slint_key_events_safely() {
        let ctrl_text = slint::SharedString::from(Key::Control);
        assert_eq!(
            hotkey_from_slint_event(&ctrl_text, true, false, false, false),
            Ok(None)
        );

        assert!(hotkey_from_slint_event("a", false, false, false, false).is_err());
        assert!(hotkey_from_slint_event(" ", false, false, false, false).is_err());

        let (_, label) = hotkey_from_slint_event(" ", false, true, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(label, "Alt+Space");

        let f8_text = slint::SharedString::from(Key::F8);
        let (_, f8_label) = hotkey_from_slint_event(&f8_text, false, false, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(f8_label, "F8");
    }

    #[test]
    fn resolve_effect_controls_truth_table() {
        assert!(!resolve_open_effect_controls(false, false));
        assert!(resolve_open_effect_controls(false, true));
        assert!(resolve_open_effect_controls(true, false));
        assert!(!resolve_open_effect_controls(true, true));
    }
}
