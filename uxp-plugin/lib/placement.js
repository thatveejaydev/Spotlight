"use strict";

// Places a project item at the playhead on the first verified free tracks.

const { mapWithConcurrency, guid } = require("./util.js");
const { isAudioFileName } = require("./media.js");

// `findProjectEntry(project, projectId, itemId)` resolves a project item, using the scan cache.
module.exports = function createPlacement(ppro, { findProjectEntry }) {
  const {
    TRACK_ITEM_READ_CONCURRENCY,
    readTicks,
    trackLayout,
    trackLayoutsEqual,
    firstSafeTrack
  } = require("./timeline.js")(ppro);

  async function sourceFootprint({ clip, item }, nested) {
    if (nested) {
      // Premiere determines a nest's routing. Reserve both track groups.
      const end = readTicks(await nested.getEndTime());
      return { video: end > 0n ? end : null, audio: end > 0n ? end : null };
    }
    const mediaTypes = ppro.Constants.MediaType;
    let path;
    try {
      path = await clip.getMediaFilePath?.();
    } catch (_) {
      // Some project items do not expose their media path.
    }
    // Recognize audio-only sources by their project filename if the media path is unavailable.
    const audioOnly = isAudioFileName(path || item.name);
    let mediaDuration = null;
    try {
      const media = await clip.getMedia?.();
      mediaDuration = readTicks(await (media?.getDuration?.() ?? media?.duration));
    } catch (_) {
      // Stills and some generated sources have no finite media duration.
    }

    async function span(type, knownPresent = false) {
      if (!knownPresent) {
        let chain;
        try {
          chain = await clip.getComponentChain(type);
        } catch (_) {
          return null;
        }
        if (chain === null) return 0n;
        if (!chain) return null;
      }
      try {
        const start = readTicks(await clip.getInPoint(type));
        const end = readTicks(await clip.getOutPoint(type));
        if (start >= 0n && end > start) return end - start;
      } catch (_) {
        // Source marks can be absent for some project items.
      }
      return mediaDuration > 0n ? mediaDuration : null;
    }
    if (audioOnly) return { video: 0n, audio: await span(mediaTypes.AUDIO, true) };
    const [video, audio] = await Promise.all([span(mediaTypes.VIDEO), span(mediaTypes.AUDIO)]);
    return { video, audio };
  }

  // Premiere may route the edit to a different track. Verify the new clip before reporting its lane.
  async function placedAudioTracks(sequence, itemId, start, frame, previousTracks) {
    const count = await sequence.getAudioTrackCount();
    const clipType = ppro.Constants.TrackItemType.CLIP;
    const matches = await Promise.all(
      Array.from({ length: count }, async (_, index) => {
        const previous = previousTracks[index];
        if (!previous || previous.locked || previous.ranges === null) return null;
        const previousRanges = previous.ranges;
        let items;
        try {
          const track = await sequence.getAudioTrack(index);
          items = await track.getTrackItems(clipType, false);
        } catch (_) {
          return null;
        }
        if (!Array.isArray(items) || items.length === 0) return null;
        const startTicks = await mapWithConcurrency(
          items,
          TRACK_ITEM_READ_CONCURRENCY,
          async (item) => {
            try {
              return readTicks(await item.getStartTime());
            } catch (_) {
              return null;
            }
          }
        );
        for (let i = 0; i < items.length; i++) {
          const itemStart = startTicks[i];
          if (itemStart === null || itemStart <= start - frame || itemStart >= start + frame) continue;
          try {
            const itemEnd = readTicks(await items[i].getEndTime());
            if (previousRanges?.some(([beforeStart, beforeEnd]) =>
              beforeStart === itemStart && beforeEnd === itemEnd)) continue;
            if ((await items[i].getProjectItem())?.getId() !== itemId) continue;
            return `A${index + 1}`;
          } catch (_) {
            // This clip cannot verify the placement.
          }
        }
        return null;
      })
    );
    return matches.filter(Boolean);
  }

  // `isCurrent()` turns false once the bridge or Premiere context changes, so a stale request never edits.
  // `knownSource` is the project entry for `payload.itemId` when the caller already has
  // it, such as a clip just imported, so the project isn't walked again to find it.
  async function placeProjectItem(payload, isCurrent, knownSource = null) {
    const project = await ppro.Project.getActiveProject();
    if (!project || guid(project) !== payload.projectId) {
      throw new Error("Active project changed. Search again.");
    }
    const sequence = await project.getActiveSequence();
    const sequenceId = guid(sequence);
    if (!sequenceId) throw new Error("Open a timeline before placing media.");
    if (sequenceId !== payload.sequenceId) throw new Error("Active sequence changed. Search again.");
    const editor = ppro.SequenceEditor?.getEditor(sequence);
    if (!editor) throw new Error("Premiere could not open the timeline editor.");

    const source = knownSource?.itemId === payload.itemId
      ? knownSource
      : await findProjectEntry(project, payload.projectId, payload.itemId);
    if (!source) throw new Error("Project item was removed. Search again.");
    if (source.bin) throw new Error("Bins can't be placed on the timeline. Bins are search only.");
    if (!source.clip) throw new Error("This project item can't be placed on the timeline.");
    if ((await sequence.getProjectItem())?.getId() === source.itemId) {
      throw new Error("A sequence cannot nest into itself.");
    }
    if (await source.clip.isOffline?.()) throw new Error("Relink the offline media before placing it.");
    const isSequence = await source.clip.isSequence();
    const nested = isSequence ? await source.clip.getSequence() : null;
    if (isSequence && !nested) throw new Error("Premiere could not resolve the source sequence.");
    if (nested && guid(nested) === sequenceId) throw new Error("A sequence cannot nest into itself.");

    const position = await sequence.getPlayerPosition();
    const start = readTicks(position);
    if (start < 0n) throw new Error("Move the playhead into the timeline before placing media.");
    const footprint = await sourceFootprint(source, nested);
    if (footprint.video === 0n && footprint.audio === 0n) {
      throw new Error("Premiere could not identify editable media in this item.");
    }
    // Video placement can align the edit to sequence frames. Round the protected end outward.
    const frame = BigInt(await sequence.getTimebase());
    if (frame <= 0n) throw new Error("Premiere could not verify the timeline frame rate.");
    const protectedStart = start / frame * frame;
    const protectedDuration = (duration) => {
      if (duration === null || duration === 0n) return duration;
      return ((start + duration + frame - 1n) / frame * frame) - protectedStart;
    };
    const scanVideo = footprint.video !== 0n;
    const scanAudio = footprint.audio !== 0n;
    const emptyLayout = { tracks: [], totalCount: 0 };
    const snapshot = ({ videoLimit = null, audioLimit = null } = {}) => Promise.all([
      scanVideo && videoLimit !== 0 ? trackLayout(sequence, "Video", videoLimit) : emptyLayout,
      scanAudio && audioLimit !== 0 ? trackLayout(sequence, "Audio", audioLimit) : emptyLayout
    ]);
    const [initialVideoLayout, initialAudioLayout] = await snapshot();
    const videoTracks = initialVideoLayout.tracks;
    const audioTracks = initialAudioLayout.tracks;
    const longest = footprint.video === null || footprint.audio === null ? null :
      (footprint.video > footprint.audio ? footprint.video : footprint.audio);
    const videoSpan = footprint.video === 0n ? 0n : longest;
    const audioSpan = footprint.audio === 0n ? 0n : longest;
    const maySpanTracks = Boolean(nested);
    const video = firstSafeTrack(videoTracks, protectedStart, protectedDuration(videoSpan), maySpanTracks);
    const audio = firstSafeTrack(
      audioTracks, protectedStart, protectedDuration(audioSpan), maySpanTracks
    );

    const videoVerifyLimit = !scanVideo ? 0
      : (!maySpanTracks && video < videoTracks.length ? video + 1 : null);
    const audioVerifyLimit = !scanAudio ? 0
      : (!maySpanTracks && audio < audioTracks.length ? audio + 1 : null);

    const confirmedFootprint = await sourceFootprint(source, nested);
    const [latestVideoLayout, latestAudioLayout] = await snapshot({
      videoLimit: videoVerifyLimit,
      audioLimit: audioVerifyLimit
    });
    if (await source.clip.isOffline?.()) throw new Error("Relink the offline media before placing it.");
    const latestProject = await ppro.Project.getActiveProject();
    const latestSequence = await latestProject?.getActiveSequence();
    const latestPlayhead = await latestSequence?.getPlayerPosition();
    if (guid(latestProject) !== payload.projectId || guid(latestSequence) !== sequenceId ||
        readTicks(latestPlayhead) !== start ||
        (scanVideo && (latestVideoLayout.totalCount !== initialVideoLayout.totalCount ||
          !trackLayoutsEqual(videoTracks.slice(0, latestVideoLayout.tracks.length), latestVideoLayout.tracks))) ||
        (scanAudio && (latestAudioLayout.totalCount !== initialAudioLayout.totalCount ||
          !trackLayoutsEqual(audioTracks.slice(0, latestAudioLayout.tracks.length), latestAudioLayout.tracks))) ||
        footprint.video !== confirmedFootprint.video || footprint.audio !== confirmedFootprint.audio) {
      throw new Error("Timeline changed before placement. Try again.");
    }
    const usesNewTrack = (scanVideo && video === videoTracks.length) ||
      (scanAudio && audio === audioTracks.length);
    let committed = false;
    project.lockedAccess(() => {
      if (!isCurrent()) {
        throw new Error("Premiere context changed before placement.");
      }
      try {
        const action = editor.createOverwriteItemAction(source.item, position, video, audio);
        if (!action) throw new Error("Premiere could not prepare a safe placement.");
        committed = project.executeTransaction((tx) => {
          if (!tx.addAction(action)) throw new Error("Premiere rejected the placement action.");
        }, `Place ${source.item.name}`);
      } catch (error) {
        if (!usesNewTrack) throw error;
        throw new Error(`Premiere could not place media on new tracks: ${error.message || error}`);
      }
    });
    if (!committed) {
      throw new Error(usesNewTrack
        ? "Premiere could not place media on new tracks. Add an empty track and retry."
        : "Premiere rejected the placement. Check track locks and media compatibility.");
    }
    const message = `Placed "${source.item.name}" at the playhead`;
    if (footprint.audio === 0n) return `${message}.`;

    const requested = `A${audio + 1}`;
    let observed = [];
    try {
      observed = await placedAudioTracks(sequence, source.itemId, start, frame, audioTracks);
    } catch (_) {
      // A read failure after commit must not turn a successful edit into a failed placement.
    }
    const location = observed.length ? ` on ${observed.join(", ")}` : "";
    const routing = observed[0] === requested ? "" : ` (requested ${requested})`;
    const firstTrack = audioTracks[0];
    const skippedA1 = audio === 0 || !firstTrack ? "" : firstTrack.locked ? " A1 is locked."
      : firstTrack.ranges === null ? " Premiere did not return readable clip or transition times for A1."
      : firstSafeTrack([firstTrack], protectedStart, protectedDuration(audioSpan), false) === 0
        ? " A1 has room, but the nested sequence needs more tracks."
        : " A1 has no verified gap at the playhead.";
    return `${message}${location}${routing}.${skippedA1}`;
  }

  return { placeProjectItem };
};
