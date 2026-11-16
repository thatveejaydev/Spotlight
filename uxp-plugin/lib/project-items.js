"use strict";

// Walks the Project panel bins. Spotlight's bin paths join names with
// BIN_PATH_SEPARATOR and use PROJECT_ROOT_BIN for top-level items; both must
// match the constants in helper-app/src/catalog.rs.

const { mapWithConcurrency } = require("./util.js");

const PROJECT_ROOT_BIN = "Project root";
const BIN_PATH_SEPARATOR = " / ";
const FOLDER_SCAN_CONCURRENCY = 8;
const ITEM_SCAN_CONCURRENCY = 24;

module.exports = function createProjectItems(ppro) {
  function inspectFolderItem(item, path) {
    const itemId = item?.getId?.();
    if (!itemId) return null;
    let child = null;
    try {
      child = ppro.FolderItem.cast(item);
    } catch (_) {
      // Older Premiere builds throw when casting a non-bin ProjectItem.
    }
    if (typeof child?.getItems === "function") {
      return {
        item,
        itemId,
        path,
        bin: true,
        childFolder: child,
        childPath: path ? `${path}${BIN_PATH_SEPARATOR}${item.name}` : item.name,
        clip: null
      };
    }
    let clip = null;
    try {
      clip = ppro.ClipProjectItem.cast(item);
    } catch (_) {
      // Some Project Panel entries cannot be edited onto a timeline.
    }
    return {
      item,
      itemId,
      path,
      bin: false,
      childFolder: null,
      childPath: null,
      clip: typeof clip?.isSequence === "function" ? clip : null
    };
  }

  async function collectProjectEntries(project, { targetItemId = null, matchesClip = null, shouldAbort = null } = {}) {
    const entries = [];
    const byId = new Map();
    const seen = new Set();
    let frontier = [{ folder: await project.getRootItem(), path: "" }];

    while (frontier.length > 0) {
      if (shouldAbort?.()) return null;
      const batches = await mapWithConcurrency(
        frontier,
        FOLDER_SCAN_CONCURRENCY,
        async ({ folder, path }) => ({
          path,
          children: (await folder?.getItems?.()) || []
        })
      );
      if (shouldAbort?.()) return null;
      const nextFrontier = [];
      for (const { path, children } of batches) {
        for (const rawItem of children) {
          const entry = inspectFolderItem(rawItem, path);
          if (!entry || seen.has(entry.itemId)) continue;
          seen.add(entry.itemId);
          if (entry.childFolder) {
            nextFrontier.push({ folder: entry.childFolder, path: entry.childPath });
          }
          const stored = {
            item: entry.item,
            itemId: entry.itemId,
            path: entry.path,
            bin: entry.bin,
            clip: entry.clip
          };
          if ((targetItemId !== null && entry.itemId === targetItemId) ||
              (entry.clip && matchesClip?.(entry.item))) {
            return { found: stored, entries, byId };
          }
          entries.push(stored);
          byId.set(entry.itemId, stored);
        }
      }
      frontier = nextFrontier;
    }

    return { found: null, entries, byId };
  }

  return {
    PROJECT_ROOT_BIN,
    ITEM_SCAN_CONCURRENCY,
    collectProjectEntries
  };
};
