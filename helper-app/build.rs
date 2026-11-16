use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    slint_build::compile("ui/spotlight.slint").expect("Failed to compile ui/spotlight.slint");
    embed_uxp_plugin();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=spotlight.rc");
        println!("cargo:rerun-if-changed=assets/spotlight.ico");
        embed_resource::compile_for("spotlight.rc", ["premiere-spotlight"], embed_resource::NONE)
            .manifest_required()
            .expect("Failed to embed the Windows icon");
    }
}

/// Lists every file the installed UXP plugin needs, so adding a module to
/// `uxp-plugin/` never requires a matching change in `installer.rs`.
fn embed_uxp_plugin() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("uxp-plugin")
        .canonicalize()
        .expect("Failed to find ../uxp-plugin");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files = Vec::new();
    collect_plugin_files(&root, &root, &mut files);
    files.sort();
    assert!(
        files.iter().any(|(path, _)| path == "manifest.json"),
        "uxp-plugin/manifest.json is missing"
    );

    let mut source = String::from("/// Files of the UXP bridge plugin: (path relative to the plugin folder, contents).\n");
    source.push_str("pub const PLUGIN_FILES: &[(&str, &str)] = &[\n");
    for (relative, absolute) in &files {
        source.push_str(&format!("    ({relative:?}, include_str!({:?})),\n", absolute.display().to_string()));
    }
    source.push_str("];\n");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("plugin_files.rs");
    fs::write(out, source).expect("Failed to write plugin_files.rs");
}

fn collect_plugin_files(root: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).expect("Failed to read uxp-plugin") {
        let path = entry.expect("Failed to read uxp-plugin entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "node_modules" && !name.starts_with('.') {
                collect_plugin_files(root, &path, files);
            }
            continue;
        }
        let shipped = [".js", ".html", ".json", ".css"].iter().any(|ext| name.ends_with(ext));
        let test = name.contains(".test.") || name == "package.json" || name == "package-lock.json";
        if shipped && !test {
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((relative, path));
        }
    }
}
