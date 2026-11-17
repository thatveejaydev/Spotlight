"use strict";

// A snapshot of the Premiere state the helper uses to dim commands that can't run.
// It reports facts only; the helper decides. A field is null when Premiere can't tell.

const { guid } = require("./util.js");

// `storage` holds the registered adjustment layers; `sourceKey(projectId)` names its key.
module.exports = function createContext(ppro, { storage, sourceKey, hostVersion }) {
  async function countSelection(sequence) {
    const items = (await (await sequence.getSelection())?.getTrackItems()) || [];
    const count = (type) => (type ? items.filter((item) => item instanceof type).length : null);
    let singleAdjustmentLayer = false;
    if (items.length === 1) {
      singleAdjustmentLayer = typeof items[0]?.isAdjustmentLayer === "function"
        ? Boolean(await items[0].isAdjustmentLayer())
        : null;
    }
    return {
      total: items.length,
      video: count(ppro.VideoClipTrackItem),
      audio: count(ppro.AudioClipTrackItem),
      singleAdjustmentLayer
    };
  }

  async function readSelection(sequence) {
    try {
      return await countSelection(sequence);
    } catch (error) {
      console.warn("[Spotlight] Could not read the Timeline selection:", error);
      return null;
    }
  }

  function readRegistered(projectId) {
    try {
      return storage ? storage.getItem(sourceKey(projectId)) != null : null;
    } catch (_) {
      return null;
    }
  }

  async function readContext() {
    const project = await ppro.Project.getActiveProject();
    const projectId = guid(project);
    const context = {
      projectId,
      sequenceId: null,
      sequenceName: null,
      selection: null,
      adjustmentLayerRegistered: null,
      hostVersion: hostVersion || null
    };
    if (!projectId) return context;
    context.adjustmentLayerRegistered = readRegistered(projectId);
    const sequence = await project.getActiveSequence();
    context.sequenceId = guid(sequence);
    if (!context.sequenceId) return context;
    context.sequenceName = typeof sequence.name === "string" ? sequence.name : null;
    context.selection = await readSelection(sequence);
    return context;
  }

  return { readContext };
};
