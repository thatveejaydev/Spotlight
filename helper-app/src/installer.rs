use serde_json::{json, Value};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const PLUGIN_ID: &str = "dev.spotlight.premiere";
const PLUGIN_NAME: &str = "Premiere Spotlight Bridge";
const PLUGIN_VERSION: &str = "0.1.0";
const PLUGIN_FOLDER_NAME: &str = "dev.spotlight.premiere_0.1.0";
const STARTUP_REG_NAME: &str = "PremiereSpotlight";
const NO_AUTO_STARTUP_MARKER: &str = ".no_auto_startup";
const UPIA_EXE_PATH: &str = r"C:\Program Files\Common Files\Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe";

const MANIFEST_JSON: &str = include_str!("../../uxp-plugin/manifest.json");
const INDEX_HTML: &str = include_str!("../../uxp-plugin/index.html");
const INDEX_JS: &str = include_str!("../../uxp-plugin/index.js");
const ACTIONS_INDEX_JS: &str = include_str!("../../uxp-plugin/actions/index.js");
const ADJUSTMENT_LAYER_JS: &str = include_str!("../../uxp-plugin/actions/adjustment-layer.js");
const REMOVE_ATTRIBUTES_JS: &str = include_str!("../../uxp-plugin/actions/remove-attributes.js");
const MARKERS_JS: &str = include_str!("../../uxp-plugin/actions/markers.js");

fn resolve_canonical_exe_path() -> Option<PathBuf> {
    let cur = env::current_exe().ok()?;
    // If running from cargo's target/release or target/debug directory, prefer the root premiere-spotlight.exe if present
    if let Some(parent) = cur.parent() {
        if parent.ends_with("target\\release") || parent.ends_with("target\\debug") {
            if let Some(project_root) = parent.parent().and_then(|p| p.parent()) {
                let root_exe = project_root.join("premiere-spotlight.exe");
                if root_exe.exists() {
                    return Some(root_exe);
                }
            }
        }
    }
    Some(cur)
}

fn uxp_plugin_dir(appdata_path: &Path) -> PathBuf {
    appdata_path
        .join("Adobe")
        .join("UXP")
        .join("Plugins")
        .join("External")
        .join(PLUGIN_FOLDER_NAME)
}

fn premiere_plugins_info_path(appdata_path: &Path) -> PathBuf {
    appdata_path
        .join("Adobe")
        .join("UXP")
        .join("PluginsInfo")
        .join("v1")
        .join("premierepro.json")
}

fn startup_shortcut_path(appdata_path: &Path) -> PathBuf {
    appdata_path
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("Premiere Spotlight.lnk")
}

fn write_uxp_plugin_and_register(exe_path: &Path, appdata_path: &Path) -> PathBuf {
    let plugin_dir = uxp_plugin_dir(appdata_path);

    // 1. Write UXP plugin files directly into %APPDATA%\Adobe\UXP\Plugins\External\dev.spotlight.premiere_0.1.0
    let actions_dir = plugin_dir.join("actions");
    if fs::create_dir_all(&actions_dir).is_ok() {
        let escaped_exe = exe_path.to_string_lossy().replace('\\', "\\\\");
        let patched_js = INDEX_JS.replace("__SPOTLIGHT_HELPER_EXE_PATH__", &escaped_exe);

        let _ = fs::write(plugin_dir.join("manifest.json"), MANIFEST_JSON);
        let _ = fs::write(plugin_dir.join("index.html"), INDEX_HTML);
        let _ = fs::write(actions_dir.join("index.js"), ACTIONS_INDEX_JS);
        let _ = fs::write(actions_dir.join("adjustment-layer.js"), ADJUSTMENT_LAYER_JS);
        let _ = fs::write(actions_dir.join("remove-attributes.js"), REMOVE_ATTRIBUTES_JS);
        let _ = fs::write(actions_dir.join("markers.js"), MARKERS_JS);
        let _ = fs::write(plugin_dir.join("index.js"), patched_js);
    }

    // 2. Register plugin as enabled in %APPDATA%\Adobe\UXP\PluginsInfo\v1\premierepro.json
    let premiere_json_path = premiere_plugins_info_path(appdata_path);
    if let Some(plugins_info_dir) = premiere_json_path.parent() {
        if fs::create_dir_all(plugins_info_dir).is_ok() {
            update_premiere_plugins_info(&premiere_json_path);
        }
    }

    plugin_dir
}

pub fn ensure_installed() {
    let Some(exe_path) = resolve_canonical_exe_path() else {
        return;
    };
    let Some(appdata) = env::var("APPDATA").ok() else {
        return;
    };

    let appdata_path = Path::new(&appdata);
    let plugin_dir = write_uxp_plugin_and_register(&exe_path, appdata_path);

    // 3. Spotlight is launched on-demand by Premiere's UXP plugin; remove any legacy Startup entry.
    let marker_path = plugin_dir.join(".installed_exe_path");
    let no_startup_marker = plugin_dir.join(NO_AUTO_STARTUP_MARKER);
    let startup_lnk = startup_shortcut_path(appdata_path);

    let exe_str = exe_path.to_string_lossy().to_string();
    let prev_marker = fs::read_to_string(&marker_path).unwrap_or_default();
    let path_changed = prev_marker.trim() != exe_str;
    let needs_startup_cleanup = !no_startup_marker.exists() || startup_lnk.exists();

    if path_changed || needs_startup_cleanup {
        let _ = fs::write(&marker_path, &exe_str);
        let _ = fs::write(&no_startup_marker, b"1");
        thread::spawn(move || {
            if needs_startup_cleanup {
                unregister_windows_startup(&startup_lnk);
            }
            if path_changed {
                try_upia_install(&exe_path);
            }
        });
    }
}

/// Synchronously installs/registers the UXP plugin for the dedicated installer.
pub fn install_for_setup(_register_startup: bool) {
    let Some(exe_path) = resolve_canonical_exe_path() else {
        return;
    };
    let Some(appdata) = env::var("APPDATA").ok() else {
        return;
    };

    let appdata_path = Path::new(&appdata);
    let plugin_dir = write_uxp_plugin_and_register(&exe_path, appdata_path);
    let startup_lnk = startup_shortcut_path(appdata_path);
    let no_startup_marker = plugin_dir.join(NO_AUTO_STARTUP_MARKER);
    let marker_path = plugin_dir.join(".installed_exe_path");
    let _ = fs::write(&marker_path, exe_path.to_string_lossy().as_bytes());
    let _ = fs::write(&no_startup_marker, b"1");
    unregister_windows_startup(&startup_lnk);

    try_upia_install(&exe_path);
}

/// Synchronously removes Spotlight's UXP plugin, startup registrations, and optionally user config.
pub fn uninstall_for_setup(purge_config: bool) {
    let Some(appdata) = env::var("APPDATA").ok() else {
        return;
    };

    let appdata_path = Path::new(&appdata);
    let startup_lnk = startup_shortcut_path(appdata_path);
    unregister_windows_startup(&startup_lnk);

    let plugin_dir = uxp_plugin_dir(appdata_path);
    if plugin_dir.exists() {
        let _ = fs::remove_dir_all(&plugin_dir);
    }

    let premiere_json_path = premiere_plugins_info_path(appdata_path);
    if premiere_json_path.exists() {
        remove_from_premiere_plugins_info(&premiere_json_path);
    }

    if purge_config {
        let config_dir = appdata_path.join("PremiereSpotlight");
        if config_dir.exists() {
            let _ = fs::remove_dir_all(&config_dir);
        }
    }

    try_upia_uninstall();
}

fn update_premiere_plugins_info(json_path: &Path) {
    let our_entry = json!({
        "hostMinVersion": "25.2.0",
        "name": PLUGIN_NAME,
        "path": format!("$localPlugins\\External\\{}", PLUGIN_FOLDER_NAME),
        "pluginId": PLUGIN_ID,
        "status": "enabled",
        "type": "uxp",
        "versionString": PLUGIN_VERSION
    });

    let mut root: Value = if let Ok(existing) = fs::read_to_string(json_path) {
        serde_json::from_str(&existing).unwrap_or_else(|_| json!({ "plugins": [] }))
    } else {
        json!({ "plugins": [] })
    };

    if !root.is_object() {
        root = json!({ "plugins": [] });
    }

    let plugins_arr = root
        .as_object_mut()
        .unwrap()
        .entry("plugins")
        .or_insert_with(|| json!([]));

    if !plugins_arr.is_array() {
        *plugins_arr = json!([]);
    }

    let arr = plugins_arr.as_array_mut().unwrap();
    let mut found = false;
    let mut changed = false;

    for item in arr.iter_mut() {
        if item.get("pluginId").and_then(|v| v.as_str()) == Some(PLUGIN_ID) {
            found = true;
            if *item != our_entry {
                *item = our_entry.clone();
                changed = true;
            }
            break;
        }
    }

    if !found {
        arr.push(our_entry);
        changed = true;
    }

    if changed || !json_path.exists() {
        if let Ok(serialized) = serde_json::to_string_pretty(&root) {
            let _ = fs::write(json_path, serialized);
        }
    }
}

fn remove_from_premiere_plugins_info(json_path: &Path) {
    let Ok(existing) = fs::read_to_string(json_path) else {
        return;
    };
    let Ok(mut root) = serde_json::from_str::<Value>(&existing) else {
        return;
    };
    let Some(arr) = root.get_mut("plugins").and_then(|v| v.as_array_mut()) else {
        return;
    };

    let before_len = arr.len();
    arr.retain(|item| item.get("pluginId").and_then(|v| v.as_str()) != Some(PLUGIN_ID));
    if arr.len() != before_len {
        if let Ok(serialized) = serde_json::to_string_pretty(&root) {
            let _ = fs::write(json_path, serialized);
        }
    }
}

fn unregister_windows_startup(startup_lnk: &Path) {
    let mut reg_cmd = Command::new("reg");
    reg_cmd.args([
        "delete",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "/v",
        STARTUP_REG_NAME,
        "/f",
    ]);
    #[cfg(windows)]
    reg_cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = reg_cmd.status();

    if startup_lnk.exists() {
        let _ = fs::remove_file(startup_lnk);
    }
}

fn try_upia_install(exe_path: &Path) {
    let upia_path = Path::new(UPIA_EXE_PATH);
    if !upia_path.exists() {
        return;
    }
    let Some(parent) = exe_path.parent() else {
        return;
    };
    let ccx_path = parent.join("premiere-spotlight.ccx");
    if !ccx_path.exists() {
        return;
    }

    let mut cmd = Command::new(upia_path);
    cmd.args(["/install", &ccx_path.to_string_lossy()]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.status();
}

fn try_upia_uninstall() {
    let upia_path = Path::new(UPIA_EXE_PATH);
    if !upia_path.exists() {
        return;
    }

    let mut cmd = Command::new(upia_path);
    cmd.args(["/remove", PLUGIN_NAME]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_json_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("premiere_spotlight_{name}_{nanos}.json"))
    }

    #[test]
    fn update_and_remove_preserves_other_plugins() {
        let path = temp_json_path("plugins_info");
        let initial = json!({
            "plugins": [
                {
                    "pluginId": "is.helm.palette.bridge",
                    "name": "Helm Bridge",
                    "status": "enabled"
                }
            ]
        });
        fs::write(&path, serde_json::to_string_pretty(&initial).unwrap()).unwrap();

        update_premiere_plugins_info(&path);
        let after_add: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = after_add["plugins"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr.iter().any(|p| p["pluginId"] == "is.helm.palette.bridge"));
        assert!(arr.iter().any(|p| p["pluginId"] == PLUGIN_ID));

        remove_from_premiere_plugins_info(&path);
        let after_remove: Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let arr = after_remove["plugins"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["pluginId"], "is.helm.palette.bridge");

        let _ = fs::remove_file(&path);
    }
}
