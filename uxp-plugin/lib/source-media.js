"use strict";

// Opens project items in the Source Monitor and resolves audio preview sources.

const { guid } = require("./util.js");

// `findProjectEntry(project, projectId, itemId)` resolves a project item, using the scan cache.
module.exports = function createSourceMedia(ppro, { findProjectEntry }) {
  const { TICKS_PER_SECOND, readTicks } = require("./timeline.js")(ppro);

  async function openInSourceMonitor(payload) {
    const monitor = ppro.SourceMonitor ?? ppro.sourceMonitor;
    if (typeof monitor?.openProjectItem !== "function") {
      throw new Error("Opening in the Source Monitor requires Premiere Pro 25.6 or later.");
    }
    const project = await ppro.Project.getActiveProject();
    if (!project || guid(project) !== payload.projectId) {
      throw new Error("Active project changed. Search again.");
    }
    const source = await findProjectEntry(project, payload.projectId, payload.itemId);
    if (!source) throw new Error("Project item was removed. Search again.");
    if (source.bin) throw new Error("Bins can't open in the Source Monitor.");
    if (!source.clip) throw new Error("This project item can't open in the Source Monitor.");
    if (!(await monitor.openProjectItem(source.item))) {
      throw new Error(`Premiere could not open "${source.item.name}" in the Source Monitor.`);
    }
    return `Opened "${source.item.name}" in the Source Monitor.`;
  }

  async function resolveAudioPreview({ projectId, itemId }) {
    const project = await ppro.Project.getActiveProject();
    if (!project || guid(project) !== projectId) {
      throw new Error("Active project changed. Search again.");
    }
    const source = await findProjectEntry(project, projectId, itemId);
    if (!source) throw new Error("Project item was removed. Search again.");
    const { clip, bin } = source;
    if (bin || !clip || await clip.isSequence()) return null;
    if (await clip.isOffline?.()) throw new Error("Relink the offline media before previewing it.");
    if (typeof clip.getMediaFilePath !== "function") {
      throw new Error("Audio preview requires Premiere Pro 25.6 or later.");
    }

    const mediaTypes = ppro.Constants.MediaType;
    async function componentChain(type) {
      try {
        return await clip.getComponentChain?.(type);
      } catch (_) {
        return undefined;
      }
    }
    const [path, audio, video] = await Promise.all([
      Promise.resolve().then(() => clip.getMediaFilePath()).catch(() => null),
      componentChain(mediaTypes.AUDIO),
      componentChain(mediaTypes.VIDEO)
    ]);
    const audioExtension = /\.(?:wav|wave|bwf|aif|aiff|aifc|flac|mp3|aac|m4a|alac)$/i;
    const audioOnly = audio && video === null;
    const standaloneAudio = audioExtension.test(path || source.item.name);
    if (video || (!audioOnly && !standaloneAudio)) {
      return null;
    }
    if (typeof path !== "string" || !path.trim()) {
      throw new Error("Premiere could not provide the audio source file.");
    }

    async function markSeconds(method) {
      try {
        const ticks = readTicks(await clip[method](mediaTypes.AUDIO));
        return ticks < 0n ? null : Number(ticks) / Number(TICKS_PER_SECOND);
      } catch (_) {
        return null;
      }
    }
    const [inSeconds, outSeconds] = await Promise.all([
      markSeconds("getInPoint"), markSeconds("getOutPoint")
    ]);
    if (guid(await ppro.Project.getActiveProject()) !== projectId) {
      throw new Error("Active project changed. Search again.");
    }
    return { path, inSeconds, outSeconds };
  }

  return { openInSourceMonitor, resolveAudioPreview };
};
