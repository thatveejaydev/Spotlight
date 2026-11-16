//! Safe replacement and recovery copies for files Spotlight edits in place.

use std::{
    ffi::OsString,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

fn sibling(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let mut name: OsString = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?
        .to_os_string();
    name.push(suffix);
    Ok(path.with_file_name(name))
}

/// Replaces `path` so readers only ever see the old or the new contents, never a partial write.
pub fn write_atomic(path: &Path, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let temp = sibling(path, ".tmp")?;
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(contents.as_ref())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Copies `path` to `<name>.bak` beside it and returns the backup's path.
pub fn back_up(path: &Path) -> io::Result<PathBuf> {
    let backup = sibling(path, ".bak")?;
    fs::copy(path, &backup)?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("spotlight_safe_file_{name}_{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_write_replaces_contents_and_leaves_no_temp_file() {
        let dir = temp_dir("write");
        let file = dir.join("data.json");
        write_atomic(&file, "first").unwrap();
        write_atomic(&file, "second").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "second");
        assert!(!dir.join("data.json.tmp").exists());

        let missing_dir = dir.join("missing").join("data.json");
        assert!(write_atomic(&missing_dir, "x").is_err());
        assert!(!dir.join("missing").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn back_up_copies_beside_the_original() {
        let dir = temp_dir("backup");
        let file = dir.join("config.json");
        fs::write(&file, "{ broken").unwrap();
        let backup = back_up(&file).unwrap();
        assert_eq!(backup, dir.join("config.json.bak"));
        assert_eq!(fs::read_to_string(&backup).unwrap(), "{ broken");
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ broken");
        let _ = fs::remove_dir_all(&dir);
    }
}
