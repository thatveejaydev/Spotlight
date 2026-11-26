//! The Library catalog: files Premiere can import from the folders the user tracks.
//!
//! A background thread walks the tracked folders and watches each one with
//! `ReadDirectoryChangesW`. Any change in a folder re-walks that folder a moment
//! later, so the index never drifts from the disk, even when Windows drops events.

use crate::catalog::{MediaType, BIN_PATH_SEPARATOR};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

/// Video formats Premiere Pro imports (Adobe's supported file formats list).
const VIDEO_EXTENSIONS: &[&str] = &[
    "3g2", "3gp", "ari", "asf", "avi", "braw", "crm", "dv", "f4v", "flv", "m1v", "m2t", "m2ts",
    "m2v", "m4v", "mov", "mp4", "mpe", "mpeg", "mpg", "mts", "mxf", "r3d", "vob", "wmv",
];
/// Audio formats Premiere Pro imports. Each one is also in the plugin's audio list
/// (`uxp-plugin/lib/media.js`), so both sides give the file the same icon.
const AUDIO_EXTENSIONS: &[&str] =
    &["aac", "aif", "aifc", "aiff", "bwf", "m4a", "mp3", "wav", "wave"];
/// Still image formats Premiere Pro imports. Each one is also in the plugin's image list.
const IMAGE_EXTENSIONS: &[&str] = &[
    "ai", "bmp", "dpx", "eps", "exr", "gif", "heic", "heif", "ico", "jfif", "jpe", "jpeg", "jpg",
    "png", "psd", "tga", "tif", "tiff",
];

/// Stops a folder such as a whole drive from flooding the palette.
const MAX_FILES_PER_FOLDER: usize = 20_000;
const MAX_DEPTH: usize = 24;
/// How long a folder stays quiet before it is walked again after a change.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);
/// How often a tracked folder that is missing is looked for again.
const MISSING_RETRY: Duration = Duration::from_secs(30);

/// The kind of media an importable file holds, or `None` when Premiere can't import it.
pub fn importable_media_type(path: &Path) -> Option<MediaType> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let extension = extension.as_str();
    if AUDIO_EXTENSIONS.contains(&extension) {
        Some(MediaType::Audio)
    } else if IMAGE_EXTENSIONS.contains(&extension) {
        Some(MediaType::Image)
    } else if VIDEO_EXTENSIONS.contains(&extension) {
        Some(MediaType::Video)
    } else {
        None
    }
}

/// A path in the form used to tell whether two paths name the same file: Windows
/// paths ignore case and accept either slash. Must match `pathKey` in
/// `uxp-plugin/lib/library-import.js`.
pub fn path_key(path: &str) -> String {
    let path = path.trim();
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// One importable file in a tracked folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    pub path: PathBuf,
    /// The file name, with its extension.
    pub name: String,
    /// The tracked folder's name and the subfolders below it, joined like bin paths.
    pub folder: String,
    pub media_type: MediaType,
    /// [`path_key`] of `path`.
    pub key: String,
}

impl LibraryEntry {
    fn new(path: PathBuf, folder: String, media_type: MediaType) -> Option<Self> {
        let name = path.file_name()?.to_str()?.to_owned();
        let key = path_key(path.to_str()?);
        Some(Self { path, name, folder, media_type, key })
    }
}

/// What the indexer found in one tracked folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderStatus {
    pub path: String,
    /// `None` when the folder is missing or can't be read.
    pub files: Option<usize>,
    /// True when the folder holds more files than Spotlight indexes.
    pub truncated: bool,
}

impl FolderStatus {
    pub fn describe(&self) -> String {
        match self.files {
            None => "Folder not found".into(),
            Some(_) if self.truncated => format!("First {MAX_FILES_PER_FOLDER} files"),
            Some(1) => "1 file".into(),
            Some(count) => format!("{count} files"),
        }
    }
}

/// Everything the indexer knows, sent to the UI thread after each change.
#[derive(Debug, Clone, Default)]
pub struct LibrarySnapshot {
    pub folders: Vec<FolderStatus>,
    pub entries: Vec<LibraryEntry>,
}

/// The name a tracked folder shows as: its last component, or the drive for a drive root.
fn folder_label(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| root.to_string_lossy().trim_end_matches(['\\', '/']).to_owned())
}

#[cfg(windows)]
fn is_hidden(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
    metadata.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

#[cfg(not(windows))]
fn is_hidden(_metadata: &std::fs::Metadata) -> bool {
    false
}

struct FolderScan {
    entries: Vec<LibraryEntry>,
    truncated: bool,
}

/// Walks `root` for importable files. Hidden and system items, dot folders and links
/// are skipped. `None` when `root` is not a readable folder.
fn scan_folder(root: &Path) -> Option<FolderScan> {
    std::fs::read_dir(root).ok()?;
    let label = folder_label(root);
    let mut entries = Vec::new();
    let mut truncated = false;
    let mut pending = vec![(root.to_path_buf(), label, 0usize)];
    while let Some((dir, folder, depth)) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&dir) else { continue };
        for child in children.flatten() {
            let Ok(file_type) = child.file_type() else { continue };
            if file_type.is_symlink() {
                continue;
            }
            let name = child.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || child.metadata().is_ok_and(|metadata| is_hidden(&metadata)) {
                continue;
            }
            let path = child.path();
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    pending.push((path, format!("{folder}{BIN_PATH_SEPARATOR}{name}"), depth + 1));
                }
            } else if let Some(media_type) = importable_media_type(&path) {
                if entries.len() >= MAX_FILES_PER_FOLDER {
                    truncated = true;
                    break;
                }
                entries.extend(LibraryEntry::new(path, folder.clone(), media_type));
            }
        }
        if truncated {
            break;
        }
    }
    entries.sort_by(|a, b| a.folder.cmp(&b.folder).then_with(|| a.name.cmp(&b.name)));
    Some(FolderScan { entries, truncated })
}

/// Joins the folders' files in folder order, keeping one entry per file when
/// tracked folders overlap.
fn build_snapshot(folders: &[PathBuf], scans: &HashMap<PathBuf, Option<FolderScan>>) -> LibrarySnapshot {
    let mut seen = HashSet::new();
    let mut snapshot = LibrarySnapshot::default();
    for folder in folders {
        let scan = scans.get(folder).and_then(Option::as_ref);
        snapshot.folders.push(FolderStatus {
            path: folder.to_string_lossy().into_owned(),
            files: scan.map(|scan| scan.entries.len()),
            truncated: scan.is_some_and(|scan| scan.truncated),
        });
        for entry in scan.into_iter().flat_map(|scan| &scan.entries) {
            if seen.insert(entry.key.clone()) {
                snapshot.entries.push(entry.clone());
            }
        }
    }
    snapshot
}

enum Message {
    SetFolders(Vec<PathBuf>),
    /// Something changed under the folder; the number is the folder list's generation.
    Changed(u64, PathBuf),
    Stop,
}

/// Owns the indexing thread. Dropping it stops the thread and its watchers.
pub struct LibraryIndexer {
    tx: Sender<Message>,
}

impl LibraryIndexer {
    /// Starts the indexing thread. `publish` receives every new snapshot on that thread.
    pub fn start(publish: impl Fn(LibrarySnapshot) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let watcher_tx = tx.clone();
        let spawned = thread::Builder::new()
            .name("library-index".into())
            .spawn(move || run_indexer(rx, watcher_tx, publish));
        if spawned.is_err() {
            crate::debug_log::log_event(
                crate::debug_log::LogKind::Error,
                "LIBRARY",
                "Could not start the library indexer.",
            );
        }
        Self { tx }
    }

    /// Replaces the tracked folders and indexes them again.
    pub fn set_folders(&self, folders: &[String]) {
        let folders = folders.iter().map(PathBuf::from).collect();
        let _ = self.tx.send(Message::SetFolders(folders));
    }
}

impl Drop for LibraryIndexer {
    fn drop(&mut self) {
        let _ = self.tx.send(Message::Stop);
    }
}

fn run_indexer(rx: Receiver<Message>, tx: Sender<Message>, publish: impl Fn(LibrarySnapshot)) {
    let mut folders: Vec<PathBuf> = Vec::new();
    let mut scans: HashMap<PathBuf, Option<FolderScan>> = HashMap::new();
    let mut watchers: Vec<watch::FolderWatch> = Vec::new();
    let mut generation = 0u64;
    let mut dirty: HashSet<PathBuf> = HashSet::new();
    let mut deadline: Option<Instant> = None;
    loop {
        let timeout = match deadline {
            Some(deadline) => deadline.saturating_duration_since(Instant::now()),
            None if scans.values().any(Option::is_none) => MISSING_RETRY,
            None => Duration::from_secs(3600),
        };
        match rx.recv_timeout(timeout) {
            Ok(Message::SetFolders(next)) => {
                generation += 1;
                watchers.clear();
                let mut unique = HashSet::new();
                folders = next.into_iter().filter(|folder| unique.insert(path_key(&folder.to_string_lossy()))).collect();
                scans.retain(|folder, _| folders.contains(folder));
                for folder in &folders {
                    let tx = tx.clone();
                    let root = folder.clone();
                    let current = generation;
                    watchers.extend(watch::FolderWatch::start(folder, move || {
                        let _ = tx.send(Message::Changed(current, root.clone()));
                    }));
                    scans.insert(folder.clone(), scan_folder(folder));
                }
                dirty.clear();
                deadline = None;
                publish(build_snapshot(&folders, &scans));
            }
            Ok(Message::Changed(changed_generation, folder)) => {
                if changed_generation == generation {
                    dirty.insert(folder);
                    deadline = Some(Instant::now() + CHANGE_DEBOUNCE);
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if deadline.take().is_none() {
                    // Look for missing folders again, and watch any that came back.
                    for folder in &folders {
                        if scans.get(folder).is_some_and(Option::is_none) && folder.is_dir() {
                            dirty.insert(folder.clone());
                            let tx = tx.clone();
                            let root = folder.clone();
                            let current = generation;
                            watchers.extend(watch::FolderWatch::start(folder, move || {
                                let _ = tx.send(Message::Changed(current, root.clone()));
                            }));
                        }
                    }
                }
                if dirty.is_empty() {
                    continue;
                }
                for folder in dirty.drain() {
                    if folders.contains(&folder) {
                        scans.insert(folder.clone(), scan_folder(&folder));
                    }
                }
                publish(build_snapshot(&folders, &scans));
            }
            Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(windows)]
mod watch {
    //! One overlapped `ReadDirectoryChangesW` loop per tracked folder. The change
    //! records are not parsed: any change, or a buffer overflow, means "walk again".

    use std::{os::windows::ffi::OsStrExt, path::Path, thread};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_NOTIFY_ENUM_DIR, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0},
        Storage::FileSystem::{
            CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
            FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE},
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        },
    };

    /// Only files and folders appearing, disappearing or being renamed change the index.
    /// Writes are left out, so a file being copied or rendered into a folder wakes the
    /// indexer once when it appears, not on every write.
    const FILTER: u32 = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME;

    /// An open folder and the read waiting on it. The read writes into `buffer` and
    /// `overlapped`, so both live on the heap and move to the watch thread unchanged.
    struct Watch {
        directory: HANDLE,
        stop_event: HANDLE,
        overlapped: Box<OVERLAPPED>,
        /// DWORD-aligned, as ReadDirectoryChangesW requires.
        buffer: Vec<u32>,
    }

    // Kernel handles and heap buffers, usable from any thread.
    unsafe impl Send for Watch {}

    impl Watch {
        /// Asks Windows for the next batch of changes. False when the folder can't be watched.
        fn read(&mut self) -> bool {
            unsafe {
                ResetEvent(self.overlapped.hEvent);
                ReadDirectoryChangesW(
                    self.directory,
                    self.buffer.as_mut_ptr().cast(),
                    (self.buffer.len() * 4) as u32,
                    1,
                    FILTER,
                    std::ptr::null_mut(),
                    &mut *self.overlapped,
                    None,
                ) != 0
            }
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.directory);
                CloseHandle(self.overlapped.hEvent);
                CloseHandle(self.stop_event);
            }
        }
    }

    pub struct FolderWatch {
        stop_event: isize,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl FolderWatch {
        /// Starts watching `folder`. Changes made after this returns are never missed.
        pub fn start(folder: &Path, on_change: impl Fn() + Send + 'static) -> Option<Self> {
            let wide: Vec<u16> = folder.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut watch = unsafe {
                let directory = CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                );
                if directory == INVALID_HANDLE_VALUE {
                    return None;
                }
                let mut overlapped: Box<OVERLAPPED> = Box::new(std::mem::zeroed());
                overlapped.hEvent = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                let stop_event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                Watch { directory, stop_event, overlapped, buffer: vec![0u32; 4096] }
            };
            if watch.overlapped.hEvent.is_null() || watch.stop_event.is_null() || !watch.read() {
                return None;
            }
            let stop_event = watch.stop_event as isize;
            let thread = thread::Builder::new()
                .name("library-watch".into())
                .spawn(move || watch_loop(watch, on_change));
            match thread {
                Ok(thread) => Some(Self { stop_event, thread: Some(thread) }),
                Err(_) => None,
            }
        }
    }

    impl Drop for FolderWatch {
        fn drop(&mut self) {
            unsafe {
                SetEvent(self.stop_event as HANDLE);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn watch_loop(mut watch: Watch, on_change: impl Fn()) {
        loop {
            let events = [watch.overlapped.hEvent, watch.stop_event];
            let woke = unsafe { WaitForMultipleObjects(2, events.as_ptr(), 0, INFINITE) };
            let mut transferred = 0u32;
            if woke != WAIT_OBJECT_0 {
                unsafe {
                    CancelIoEx(watch.directory, &*watch.overlapped);
                    GetOverlappedResult(watch.directory, &*watch.overlapped, &mut transferred, 1);
                }
                return;
            }
            let ok = unsafe { GetOverlappedResult(watch.directory, &*watch.overlapped, &mut transferred, 0) };
            on_change();
            let overflowed = ok == 0 && unsafe { GetLastError() } == ERROR_NOTIFY_ENUM_DIR;
            if (ok == 0 && !overflowed) || !watch.read() {
                // The folder went away or can't be watched; the indexer looks for it again.
                return;
            }
        }
    }
}

#[cfg(not(windows))]
mod watch {
    pub struct FolderWatch;

    impl FolderWatch {
        pub fn start(_folder: &std::path::Path, _on_change: impl Fn() + Send + 'static) -> Option<Self> {
            None
        }
    }
}

/// Shows Windows' folder picker, owned by `owner` so it stays above the palette, and
/// returns the chosen folders. `None` when the user cancels. Blocks until it closes.
#[cfg(windows)]
pub fn pick_folders(owner: isize) -> Option<Vec<PathBuf>> {
    use raw_window_handle::{
        DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle,
        Win32WindowHandle, WindowHandle,
    };

    struct Owner(std::num::NonZeroIsize);

    impl HasWindowHandle for Owner {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let raw = RawWindowHandle::Win32(Win32WindowHandle::new(self.0));
            // The palette window outlives the dialog it owns.
            Ok(unsafe { WindowHandle::borrow_raw(raw) })
        }
    }

    impl HasDisplayHandle for Owner {
        fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
            Ok(DisplayHandle::windows())
        }
    }

    let mut dialog = rfd::FileDialog::new().set_title("Add Library folders");
    if let Some(owner) = std::num::NonZeroIsize::new(owner) {
        dialog = dialog.set_parent(&Owner(owner));
    }
    dialog.pick_folders()
}

#[cfg(not(windows))]
pub fn pick_folders(_owner: isize) -> Option<Vec<PathBuf>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spotlight-library-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn importable_files_get_the_icon_the_plugin_gives_them() {
        let media = include_str!("../../uxp-plugin/lib/media.js");
        let list = |name: &str| -> Vec<String> {
            media
                .split(&format!("const {name} = ["))
                .nth(1)
                .and_then(|rest| rest.split(']').next())
                .unwrap()
                .split(',')
                .map(|entry| entry.trim().trim_matches('"').to_owned())
                .filter(|entry| !entry.is_empty())
                .collect()
        };
        let (audio, image) = (list("AUDIO_FILE_EXTENSIONS"), list("IMAGE_FILE_EXTENSIONS"));
        for extension in AUDIO_EXTENSIONS {
            assert!(audio.iter().any(|known| known == extension), "{extension} is not audio in media.js");
        }
        for extension in IMAGE_EXTENSIONS {
            assert!(image.iter().any(|known| known == extension), "{extension} is not an image in media.js");
        }
        for extension in VIDEO_EXTENSIONS {
            assert!(!audio.contains(&extension.to_string()) && !image.contains(&extension.to_string()));
        }
        assert_eq!(importable_media_type(Path::new("Boom.WAV")), Some(MediaType::Audio));
        assert_eq!(importable_media_type(Path::new("meme.png")), Some(MediaType::Image));
        assert_eq!(importable_media_type(Path::new("clip.MP4")), Some(MediaType::Video));
        for name in ["notes.txt", "project.prproj", "song.flac", "noextension"] {
            assert_eq!(importable_media_type(Path::new(name)), None, "{name}");
        }
    }

    #[test]
    fn path_keys_ignore_case_slashes_and_the_long_path_prefix() {
        assert_eq!(path_key(r"D:\SFX\Boom.wav"), path_key("d:/sfx/boom.WAV"));
        assert_eq!(path_key(r"\\?\D:\SFX\Boom.wav"), r"d:\sfx\boom.wav");
        assert_ne!(path_key(r"D:\SFX\Boom.wav"), path_key(r"D:\SFX\Boom 2.wav"));
    }

    #[test]
    fn walks_subfolders_for_importable_files_and_skips_the_rest() {
        let root = temp_dir("walk");
        fs::create_dir_all(root.join("Whooshes/Fast")).unwrap();
        fs::create_dir_all(root.join(".cache")).unwrap();
        for file in ["Boom.wav", "readme.txt", "Whooshes/Whoosh_01.wav", "Whooshes/Fast/Swish.mp3",
            "Whooshes/Fast/shot.png", ".cache/hidden.wav"] {
            fs::write(root.join(file), b"").unwrap();
        }
        let scan = scan_folder(&root).unwrap();
        let label = folder_label(&root);
        let found: Vec<_> = scan.entries.iter().map(|e| (e.folder.clone(), e.name.clone(), e.media_type)).collect();
        assert_eq!(found, [
            (label.clone(), "Boom.wav".to_string(), MediaType::Audio),
            (format!("{label} / Whooshes"), "Whoosh_01.wav".to_string(), MediaType::Audio),
            (format!("{label} / Whooshes / Fast"), "Swish.mp3".to_string(), MediaType::Audio),
            (format!("{label} / Whooshes / Fast"), "shot.png".to_string(), MediaType::Image),
        ]);
        assert!(!scan.truncated);
        assert!(scan_folder(&root.join("missing")).is_none());

        // Overlapping folders list each file once, and a missing folder says so.
        let inner = root.join("Whooshes");
        let folders = vec![root.clone(), inner.clone(), root.join("missing")];
        let scans: HashMap<_, _> = folders.iter().map(|f| (f.clone(), scan_folder(f))).collect();
        let snapshot = build_snapshot(&folders, &scans);
        assert_eq!(snapshot.entries.len(), 4);
        assert_eq!(snapshot.folders.iter().map(|f| f.files).collect::<Vec<_>>(), [Some(4), Some(3), None]);
        assert_eq!(snapshot.folders[2].describe(), "Folder not found");
        assert_eq!(snapshot.folders[1].describe(), "3 files");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_indexer_publishes_folders_and_picks_up_new_files() {
        let root = temp_dir("watch");
        fs::write(root.join("first.wav"), b"").unwrap();
        let (tx, rx) = mpsc::channel();
        let indexer = LibraryIndexer::start(move |snapshot| {
            let _ = tx.send(snapshot);
        });
        indexer.set_folders(&[root.to_string_lossy().into_owned()]);
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(first.entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["first.wav"]);
        if cfg!(windows) {
            fs::write(root.join("second.mp3"), b"").unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut names = Vec::new();
            while Instant::now() < deadline && names.len() < 2 {
                if let Ok(snapshot) = rx.recv_timeout(Duration::from_millis(500)) {
                    names = snapshot.entries.iter().map(|e| e.name.clone()).collect();
                }
            }
            names.sort();
            assert_eq!(names, ["first.wav", "second.mp3"]);
        }
        indexer.set_folders(&[]);
        assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().entries.is_empty());
        drop(indexer);
        let _ = fs::remove_dir_all(&root);
    }
}
