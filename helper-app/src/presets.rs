use crate::{
    bridge::{find_latest_premiere_prefs_file, read_preset_search_shortcuts, ShortcutSpec},
    catalog::{EffectEntry, EffectType, BIN_PATH_SEPARATOR},
    error_toast::ErrorToast,
    win_focus,
};
use roxmltree::{Document, Node};
use slint::{SharedString, Timer, TimerMode};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    fs, io,
    path::PathBuf,
    rc::Rc,
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime},
};

const LIBRARY_FILENAME: &str = "Effect Presets and Custom Items.prfpset";

fn child<'a, 'input>(node: Node<'a, 'input>, tag: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|node| node.has_tag_name(tag))
}

fn item_refs<'a, 'input>(bin: Node<'a, 'input>) -> impl Iterator<Item = &'a str> + use<'a, 'input> {
    child(bin, "Items")
        .into_iter()
        .flat_map(|items| items.children())
        .filter(|node| node.has_tag_name("Item"))
        .filter_map(|item| item.attribute("ObjectRef"))
}

fn parse_library(xml: &str) -> Option<Vec<EffectEntry>> {
    let document = Document::parse(xml).ok()?;
    let root = document.root_element();
    if !root.has_tag_name("PremiereData") {
        return None;
    }
    let objects: HashMap<_, _> = root
        .children()
        .filter_map(|node| Some((node.attribute("ObjectID")?, node)))
        .collect();
    let tree = objects.get(child(root, "Tree")?.attribute("ObjectRef")?)?;
    let root_bin = objects.get(child(*tree, "RootBin")?.attribute("ObjectRef")?)?;
    let mut presets = Vec::new();
    let mut visited = HashSet::new();
    let mut names_and_folders = HashSet::new();
    let mut stack: Vec<_> = item_refs(*root_bin)
        .filter_map(|id| {
            let bin = *objects.get(id)?;
            let base = child(bin, "TreeItemBase")?;
            let properties = child(child(base, "Node")?, "Properties")?;
            child(properties, "HandlerEffects.EffectItemTree.PresetsBin")?;
            let folder = child(base, "Name")?.text()?;
            Some((id, folder.to_owned()))
        })
        .collect();

    while let Some((id, folder)) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Some(node) = objects.get(id).copied() else {
            continue;
        };
        for id in item_refs(node) {
            let Some(item) = objects.get(id).copied() else {
                continue;
            };
            let Some(base) = child(item, "TreeItemBase") else {
                continue;
            };
            let Some(name) = child(base, "Name").and_then(|name| name.text()) else {
                continue;
            };
            if item.has_tag_name("BinTreeItem") {
                stack.push((id, format!("{folder}{BIN_PATH_SEPARATOR}{name}")));
                continue;
            }
            let is_preset = child(base, "Data")
                .and_then(|data| data.attribute("ObjectRef"))
                .and_then(|id| objects.get(id))
                .is_some_and(|data| data.has_tag_name("FilterPresetItem"));
            if !item.has_tag_name("TreeItem")
                || !is_preset
                || name.trim().is_empty()
                || name.chars().any(char::is_control)
                || !names_and_folders.insert((name, folder.clone()))
            {
                continue;
            }
            presets.push(EffectEntry {
                name: name.to_owned(),
                match_name: format!("preset:{}:{folder}{name}", folder.len()),
                effect_type: EffectType::Preset,
                folder: folder.clone(),
            });
        }
    }
    Some(presets)
}

#[derive(Default)]
struct PresetLibrary {
    path: Option<PathBuf>,
    stamp: Option<(SystemTime, u64)>,
}

impl PresetLibrary {
    fn refresh(&mut self, path: Option<PathBuf>) -> Option<Vec<EffectEntry>> {
        let profile_changed = self.path != path;
        if profile_changed {
            self.path = path;
            self.stamp = None;
        }
        let Some(path) = self.path.as_ref() else {
            return profile_changed.then(Vec::new);
        };
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return (self.stamp.take().is_some() || profile_changed).then(Vec::new);
            }
            Err(_) => return profile_changed.then(Vec::new),
        };
        let Ok(modified) = metadata.modified() else {
            return profile_changed.then(Vec::new);
        };
        let stamp = (modified, metadata.len());
        if self.stamp == Some(stamp) {
            return None;
        }
        // Premiere can replace the file while saving. Retry a failed read on the
        // next poll and keep the previous complete snapshot for the same profile.
        match fs::read_to_string(path)
            .ok()
            .and_then(|xml| parse_library(&xml))
        {
            Some(presets) => {
                self.stamp = Some(stamp);
                Some(presets)
            }
            None => profile_changed.then(Vec::new),
        }
    }
}

pub(crate) fn watch_library(mut on_change: impl FnMut(Vec<EffectEntry>) -> bool + Send + 'static) {
    thread::spawn(move || {
        let mut library = PresetLibrary::default();
        loop {
            let path = find_latest_premiere_prefs_file()
                .map(|prefs| prefs.with_file_name(LIBRARY_FILENAME));
            if let Some(presets) = library.refresh(path) {
                if !on_change(presets) {
                    break;
                }
            }
            thread::sleep(Duration::from_secs(2));
        }
    });
}

#[derive(Debug)]
pub(crate) struct PresetHandoff {
    target: isize,
    process_id: u32,
    entry: Arc<EffectEntry>,
    effects: ShortcutSpec,
    find_box: ShortcutSpec,
}

#[derive(Clone, Copy)]
enum ChordStep {
    HoldModifiers,
    PressKey,
    ReleaseModifiers,
}

thread_local! {
    static HANDOFF_TIMER: Timer = Timer::default();
    static HANDOFF_PROCESS: Cell<Option<u32>> = const { Cell::new(None) };
}

fn stop_handoff() {
    HANDOFF_PROCESS.set(None);
    HANDOFF_TIMER.with(Timer::stop);
    win_focus::release_preset_modifiers();
}

#[cfg(windows)]
pub(crate) fn foreground_changed(hwnd: isize) {
    if HANDOFF_PROCESS
        .get()
        .is_some_and(|process| win_focus::window_process_id(hwnd) != Some(process))
    {
        stop_handoff();
    }
}

impl PresetHandoff {
    pub(crate) fn new(target: isize, entry: Arc<EffectEntry>) -> Result<Self, SharedString> {
        let unavailable = || SharedString::from("Open Spotlight from Premiere to find a preset.");
        let executable = win_focus::premiere_executable(target).ok_or_else(unavailable)?;
        let process_id = win_focus::window_process_id(target).ok_or_else(unavailable)?;
        let (effects, find_box) = read_preset_search_shortcuts(&executable)?;
        Ok(Self {
            target,
            process_id,
            entry,
            effects,
            find_box,
        })
    }

    pub(crate) fn start(self, toast: &Rc<ErrorToast>) {
        let toast = Rc::downgrade(toast);
        let shortcuts = [
            self.effects,
            self.find_box,
            ShortcutSpec {
                vk: b'A' as u16,
                ctrl: true,
                alt: false,
                shift: false,
            },
        ];
        let index = Cell::new(0);
        let step = Cell::new(ChordStep::HoldModifiers);
        let started = Instant::now();
        let ready_at = Cell::new(started);
        stop_handoff();
        HANDOFF_TIMER.with(|timer| {
            timer.start(TimerMode::Repeated, Duration::from_millis(20), move || {
                if win_focus::window_process_id(win_focus::get_foreground_hwnd())
                    != Some(self.process_id)
                {
                    stop_handoff();
                    return;
                }
                let now = Instant::now();
                let timed_out = now.duration_since(started) >= Duration::from_secs(2);
                if !timed_out && now < ready_at.get() {
                    return;
                }

                if !timed_out {
                    if let Some(&shortcut) = shortcuts.get(index.get()) {
                        match step.get() {
                            ChordStep::HoldModifiers => {
                                if !win_focus::preset_handoff_keys_released() {
                                    return;
                                }
                                if win_focus::begin_preset_shortcut(self.process_id, shortcut) {
                                    HANDOFF_PROCESS.set(Some(self.process_id));
                                    step.set(ChordStep::PressKey);
                                    ready_at.set(now + Duration::from_millis(40));
                                    return;
                                }
                            }
                            ChordStep::PressKey
                                if win_focus::press_preset_shortcut(self.process_id, shortcut) =>
                            {
                                // Premiere can sample modifier state while processing the key.
                                step.set(ChordStep::ReleaseModifiers);
                                ready_at.set(now + Duration::from_millis(140));
                                return;
                            }
                            ChordStep::ReleaseModifiers => {
                                win_focus::release_preset_modifiers();
                                index.set(index.get() + 1);
                                step.set(ChordStep::HoldModifiers);
                                ready_at.set(now + Duration::from_millis(40));
                                return;
                            }
                            _ => {}
                        }
                    } else {
                        if win_focus::type_effects_search(self.process_id, &self.entry.name) {
                            stop_handoff();
                            return;
                        }
                    }
                }
                stop_handoff();
                if let Some(toast) = toast.upgrade() {
                    toast.show(
                        format!(
                            "Search for “{}” in Premiere’s Effects panel.",
                            self.entry.name
                        )
                        .into(),
                        self.target,
                    );
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CatalogStore, SearchResult};

    const LIBRARY: &str = r#"
<PremiereData>
    <Tree ObjectRef="1"/>
    <Tree ObjectID="1"><RootBin ObjectRef="2"/></Tree>
    <BinTreeItem ObjectID="2"><Items><Item ObjectRef="3"/><Item ObjectRef="8"/></Items></BinTreeItem>
    <BinTreeItem ObjectID="3">
        <TreeItemBase><Name>Presets</Name><Node><Properties>
            <HandlerEffects.EffectItemTree.PresetsBin>1</HandlerEffects.EffectItemTree.PresetsBin>
        </Properties></Node></TreeItemBase>
        <Items><Item ObjectRef="4"/><Item ObjectRef="5"/><Item ObjectRef="7"/></Items>
    </BinTreeItem>
    <TreeItem ObjectID="4"><TreeItemBase><Name>Café &amp; 光 🎞</Name><Data ObjectRef="6"/></TreeItemBase></TreeItem>
    <BinTreeItem ObjectID="5">
        <TreeItemBase><Name>Pack</Name></TreeItemBase>
        <Items><Item ObjectRef="4"/><Item ObjectRef="5"/></Items>
    </BinTreeItem>
    <FilterPresetItem ObjectID="6"><Name>Internal name</Name></FilterPresetItem>
    <TreeItem ObjectID="7"><TreeItemBase><Name>Effect shortcut</Name><Data ObjectRef="9"/></TreeItemBase></TreeItem>
    <BinTreeItem ObjectID="8">
        <TreeItemBase><Name>Other custom items</Name></TreeItemBase>
        <Items><Item ObjectRef="10"/></Items>
    </BinTreeItem>
    <VideoFilterComponent ObjectID="9"/>
    <TreeItem ObjectID="10"><TreeItemBase><Name>Outside presets</Name><Data ObjectRef="6"/></TreeItemBase></TreeItem>
</PremiereData>
"#;

    #[test]
    fn custom_presets_are_searchable_by_display_name_and_folder() {
        let mut catalog = CatalogStore::new_with_defaults();
        catalog.set_presets(parse_library(LIBRARY).unwrap());
        for scope in ["all", "effects"] {
            let results = catalog.search("Café & 光 🎞", scope);
            assert_eq!(results.len(), 2);
            let [SearchResult::Effect(root), SearchResult::Effect(nested)] = &results[..] else {
                panic!("Expected two presets");
            };
            assert_eq!(root.name, "Café & 光 🎞");
            assert_eq!(root.category_short(), "Preset");
            assert_eq!(root.folder, "Presets");
            assert_eq!(nested.folder, "Presets / Pack");
            assert!(!results[0].same_item(&results[1]));
        }
        assert!(catalog.search("Café & 光 🎞", "project").is_empty());
        assert!(catalog.search("Outside presets", "effects").is_empty());
        assert!(catalog
            .search("Café & 光 🎞", "effects")
            .iter()
            .all(|item| { matches!(item, SearchResult::Effect(entry) if entry.is_preset()) }));

        catalog.set_catalog(vec![EffectEntry {
            name: "Gaussian Blur".into(),
            match_name: "builtin.blur".into(),
            effect_type: "videoEffect".into(),
            folder: "Blur & Sharpen".into(),
        }]);
        assert_eq!(catalog.search("Café & 光 🎞", "effects").len(), 2);
        catalog.set_presets(Vec::new());
        assert_eq!(catalog.search("Gaussian Blur", "effects").len(), 1);
    }

    #[test]
    fn library_refreshes_after_saves_and_ignores_incomplete_rewrites() -> io::Result<()> {
        let path = std::env::temp_dir().join(format!(
            "premiere-spotlight-presets-{}-{}.prfpset",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut library = PresetLibrary::default();
        fs::write(&path, LIBRARY)?;
        assert_eq!(library.refresh(Some(path.clone())).unwrap().len(), 2);
        assert!(library.refresh(Some(path.clone())).is_none());
        fs::write(&path, "<PremiereData>")?;
        assert!(library.refresh(Some(path.clone())).is_none());
        fs::write(&path, LIBRARY.replace("Café &amp; 光 🎞", "Renamed preset"))?;
        let updated = library.refresh(Some(path.clone())).unwrap();
        assert!(updated.iter().all(|entry| entry.name == "Renamed preset"));
        fs::remove_file(&path)?;
        assert!(library.refresh(Some(path.clone())).unwrap().is_empty());
        assert!(library.refresh(Some(path)).is_none());
        Ok(())
    }
}
