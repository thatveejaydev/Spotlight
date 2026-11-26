"use strict";

// Imports a Library file into the project, or finds the clip already made from it,
// so the helper can place it or open it like any other project item.

const { mapWithConcurrency } = require("./util.js");

// New imports land in this bin at the top of the Project panel.
const LIBRARY_BIN = "Spotlight Library";
const PATH_READ_CONCURRENCY = 24;
// importFiles can resolve before the clip shows up, so the bin is checked a few times.
const IMPORT_POLL_MS = 100;
const IMPORT_POLL_TRIES = 20;

// The form used to tell whether two paths name the same file: Windows paths ignore
// case and accept either slash. Must match path_key in helper-app/src/library.rs.
function pathKey(path) {
  let text = String(path || "").trim();
  if (text.startsWith("\\\\?\\")) text = text.slice(4);
  return text.replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();
}

function fileName(path) {
  return String(path).split(/[\\/]/).pop() || String(path);
}

const defaultDelay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// `collectProjectEntries(project)` walks every bin, as lib/project-items.js does.
// `cachedClipFor(project, key)` answers from the last project scan: the entry for the
// clip made from file `key`, null when the scan found none, or undefined when there is
// no scan of this project to ask.
module.exports = function createLibraryImport(ppro, {
  collectProjectEntries,
  cachedClipFor = () => undefined,
  delay = defaultDelay
}) {
  async function mediaPath(clip) {
    try {
      return (await clip?.getMediaFilePath?.()) || null;
    } catch (_) {
      return null;
    }
  }

  // The first clip, in Project panel order, whose file is `key`.
  async function firstClipFor(entries, key) {
    const clips = entries.filter((entry) => entry.clip && !entry.bin);
    const paths = await mapWithConcurrency(clips, PATH_READ_CONCURRENCY, (entry) => mediaPath(entry.clip));
    const index = paths.findIndex((path) => path && pathKey(path) === key);
    return index < 0 ? null : clips[index];
  }

  async function findInProject(project, key) {
    const collected = await collectProjectEntries(project);
    return collected ? firstClipFor(collected.entries, key) : null;
  }

  // The clip already made from file `key`. The last project scan answers without walking
  // the bins; a clip it names is checked against Premiere first, since it may have been
  // removed or relinked since.
  async function findExisting(project, key) {
    const cached = cachedClipFor(project, key);
    if (cached === null) return null;
    if (cached && cached.item?.getId?.() === cached.itemId && pathKey(await mediaPath(cached.clip)) === key) {
      return cached;
    }
    return findInProject(project, key);
  }

  // The clips directly inside `bin`, in the shape collectProjectEntries returns.
  async function binEntries(bin) {
    let folder = null;
    try {
      folder = ppro.FolderItem.cast(bin);
    } catch (_) {
      return [];
    }
    const items = (await folder?.getItems?.()) || [];
    return items.map((item) => {
      let clip = null;
      try {
        clip = ppro.ClipProjectItem.cast(item);
      } catch (_) {
        // Not a clip.
      }
      return { item, itemId: item?.getId?.(), bin: false, clip: typeof clip?.isSequence === "function" ? clip : null };
    }).filter((entry) => entry.itemId);
  }

  async function findLibraryBin(root) {
    for (const item of (await root.getItems?.()) || []) {
      if (item?.name !== LIBRARY_BIN) continue;
      try {
        if (typeof ppro.FolderItem.cast(item)?.getItems === "function") return item;
      } catch (_) {
        // A clip with the bin's name.
      }
    }
    return null;
  }

  // The Spotlight Library bin, created on first use. Null when Premiere can't make it,
  // so the file goes to the top of the project instead.
  async function libraryBin(project) {
    const root = await project.getRootItem();
    const existing = await findLibraryBin(root);
    if (existing || typeof root?.createBinAction !== "function") return existing;
    try {
      let created = false;
      project.lockedAccess(() => {
        const action = root.createBinAction(LIBRARY_BIN, true);
        created = Boolean(action) && project.executeTransaction((tx) => {
          tx.addAction(action);
        }, `Create ${LIBRARY_BIN} bin`);
      });
      return created ? findLibraryBin(root) : null;
    } catch (_) {
      return null;
    }
  }

  // Returns `{ entry, imported }`: the project clip for `path`, and whether this call imported it.
  async function importLibraryFile(project, path) {
    const key = pathKey(path);
    if (!key) throw new Error("This Library file has no path.");
    const name = fileName(path);
    const existing = await findExisting(project, key);
    if (existing) return { entry: existing, imported: false };
    if (typeof project.importFiles !== "function") {
      throw new Error("Importing Library files requires Premiere Pro 25.0 or later.");
    }

    const bin = await libraryBin(project);
    let accepted;
    try {
      accepted = bin
        ? await project.importFiles([path], true, bin, false)
        : await project.importFiles([path], true);
    } catch (error) {
      throw new Error(`Premiere could not import "${name}": ${error?.message || error}`);
    }
    // importFiles can report success without importing anything, so look for the clip.
    for (let attempt = 0; attempt < IMPORT_POLL_TRIES; attempt++) {
      const found = bin ? await firstClipFor(await binEntries(bin), key) : null;
      const entry = found || (attempt % 5 === 4 || !bin ? await findInProject(project, key) : null);
      if (entry) return { entry, imported: true };
      await delay(IMPORT_POLL_MS);
    }
    throw new Error(accepted === false
      ? `Premiere could not import "${name}".`
      : `Premiere did not add "${name}" to the project. Check that Premiere supports the file.`);
  }

  return { LIBRARY_BIN, importLibraryFile };
};

module.exports.pathKey = pathKey;
