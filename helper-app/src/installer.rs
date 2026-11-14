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
const PLUGIN_VERSION: &str = "0.1.0";
const PLUGIN_FOLDER_NAME: &str = "dev.spotlight.premiere_0.1.0";

const MANIFEST_JSON: &str = include_str!("../../uxp-plugin/manifest.json");
const INDEX_HTML: &str = include_str!("../../uxp-plugin/index.html");
const INDEX_JS: &str = include_str!("../../uxp-plugin/index.js");

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

pub fn ensure_installed() {
    let Some(exe_path) = resolve_canonical_exe_path() else {
        return;
    };
    let Some(appdata) = env::var("APPDATA").ok() else {
        return;
    };

    let appdata_path = Path::new(&appdata);
    let plugin_dir = appdata_path
        .join("Adobe")
        .join("UXP")
        .join("Plugins")
        .join("External")
        .join(PLUGIN_FOLDER_NAME);

    // 1. Write UXP plugin files directly into %APPDATA%\Adobe\UXP\Plugins\External\dev.spotlight.premiere_0.1.0
    if fs::create_dir_all(&plugin_dir).is_ok() {
        let escaped_exe = exe_path.to_string_lossy().replace('\\', "\\\\");
        let patched_js = INDEX_JS.replace("__SPOTLIGHT_HELPER_EXE_PATH__", &escaped_exe);

        let _ = fs::write(plugin_dir.join("manifest.json"), MANIFEST_JSON);
        let _ = fs::write(plugin_dir.join("index.html"), INDEX_HTML);
        let _ = fs::write(plugin_dir.join("index.js"), patched_js);
    }

    // 2. Register plugin as enabled in %APPDATA%\Adobe\UXP\PluginsInfo\v1\premierepro.json
    let plugins_info_dir = appdata_path
        .join("Adobe")
        .join("UXP")
        .join("PluginsInfo")
        .join("v1");
    if fs::create_dir_all(&plugins_info_dir).is_ok() {
        let premiere_json_path = plugins_info_dir.join("premierepro.json");
        update_premiere_plugins_info(&premiere_json_path);
    }

    // 3. Ensure Windows Startup registration (--watch) and optional UPIA install in background
    let marker_path = plugin_dir.join(".installed_exe_path");
    let startup_lnk = appdata_path
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("Premiere Spotlight.lnk");

    let exe_str = exe_path.to_string_lossy().to_string();
    let prev_marker = fs::read_to_string(&marker_path).unwrap_or_default();
    let needs_startup_sync = prev_marker.trim() != exe_str || !startup_lnk.exists();

    if needs_startup_sync {
        let _ = fs::write(&marker_path, &exe_str);
        thread::spawn(move || {
            register_windows_startup(&exe_path, &startup_lnk);
            try_upia_install(&exe_path);
        });
    }
}

fn update_premiere_plugins_info(json_path: &Path) {
    let our_entry = json!({
        "hostMinVersion": "25.2.0",
        "name": "Premiere Spotlight Bridge",
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

fn register_windows_startup(exe_path: &Path, startup_lnk: &Path) {
    let exe_str = exe_path.to_string_lossy();
    let work_dir = exe_path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    // 1. Register HKCU\Software\Microsoft\Windows\CurrentVersion\Run
    let run_val = format!("\"{}\" --watch", exe_str);
    let mut reg_cmd = Command::new("reg");
    reg_cmd.args([
        "add",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
        "/v",
        "PremiereSpotlight",
        "/t",
        "REG_SZ",
        "/d",
        &run_val,
        "/f",
    ]);
    #[cfg(windows)]
    reg_cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = reg_cmd.status();

    // 2. Create Startup Folder .lnk shortcut (same mechanism as Cockpit's register-helper.ps1)
    let ps_script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{}'); \
         $s.TargetPath = '{}'; \
         $s.Arguments = '--watch'; \
         $s.WorkingDirectory = '{}'; \
         $s.Description = 'Premiere Spotlight Helper'; \
         $s.Save()",
        startup_lnk.to_string_lossy().replace('\'', "''"),
        exe_str.replace('\'', "''"),
        work_dir.replace('\'', "''"),
    );

    let mut ps_cmd = Command::new("powershell");
    ps_cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        &ps_script,
    ]);
    #[cfg(windows)]
    ps_cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = ps_cmd.status();
}

fn try_upia_install(exe_path: &Path) {
    let upia_path = Path::new(
        r"C:\Program Files\Common Files\Adobe\Adobe Desktop Common\RemoteComponents\UPI\UnifiedPluginInstallerAgent\UnifiedPluginInstallerAgent.exe",
    );
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
