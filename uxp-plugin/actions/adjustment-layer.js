"use strict";

const SETUP_MESSAGE =
  "Create an adjustment layer in Premiere, place and select it on the timeline, then run Register Adjustment Layer.";
const TIMELINE_CHANGED = "The timeline changed. Run Add Adjustment Layer again.";
const SHORT_LAYER = "Premiere added a short adjustment layer. Extend its end on the timeline.";

function sourceKey(projectId) {
  return `spotlight.adjustmentLayer.${projectId}`;
}

function sourceIdentity(ppro, item) {
  const id = item?.getId?.();
  if (!id) return null;
  const uniqueId = ppro.UniqueSerializeable?.cast(item)?.getUniqueID()?.toString();
  return uniqueId ? `guid:${uniqueId}` : `id:${id}`;
}

async function activeTimeline(ppro) {
  const project = await ppro.Project.getActiveProject();
  const sequence = await project?.getActiveSequence();
  const projectId = project?.guid?.toString();
  const sequenceId = sequence?.guid?.toString();
  if (!projectId || !sequenceId) throw new Error("Open a sequence in the Timeline first.");
  return { project, sequence, projectId, sequenceId };
}

async function selectionItems(sequence) {
  return await (await sequence.getSelection())?.getTrackItems() || [];
}

async function registerAdjustmentLayer(ppro, isCurrent, { storage, findSource }) {
  const { project, sequence, projectId, sequenceId } = await activeTimeline(ppro);
  const [selected, ...otherItems] = await selectionItems(sequence);
  if (otherItems.length || !await selected?.isAdjustmentLayer?.()) {
    throw new Error("Select exactly one adjustment layer in the timeline to register it.");
  }
  const identity = sourceIdentity(ppro, await selected.getProjectItem());
  if (!identity || !await findSource(project, (item) => sourceIdentity(ppro, item) === identity)) {
    throw new Error("Premiere could not find the selected adjustment layer in this project's bins.");
  }
  const current = await activeTimeline(ppro);
  const [confirmed, ...remaining] = await selectionItems(current.sequence);
  if (current.projectId !== projectId || current.sequenceId !== sequenceId ||
      remaining.length || !await confirmed?.isAdjustmentLayer?.() ||
      sourceIdentity(ppro, await confirmed.getProjectItem()) !== identity || !isCurrent()) {
    throw new Error("The timeline selection changed. Run Register Adjustment Layer again.");
  }
  storage.setItem(sourceKey(projectId), identity);
  return "Registered the adjustment layer for this project.";
}

async function selectedVideoRanges(ppro, sequence, readItemRange) {
  const items = await selectionItems(sequence);
  const ranges = await Promise.all(items.map(async (item) => {
    if (typeof item?.isAdjustmentLayer !== "function" ||
        (item.getType && await item.getType() !== ppro.Constants.TrackItemType.CLIP)) return null;
    const [trackIndex, [start, end], projectItem] = await Promise.all([
      item.getTrackIndex(), readItemRange(item), item.getProjectItem()
    ]);
    const itemId = projectItem?.getId();
    if (!Number.isSafeInteger(trackIndex) || trackIndex < 0 || start < 0n || end <= start || !itemId) {
      throw new Error("Premiere could not verify the selected video clips.");
    }
    return { trackIndex, start, end, itemId };
  }));
  return ranges.filter(Boolean);
}

function sameSelection(before, after) {
  return before.length === after.length && before.every((clip) => after.some((other) =>
    clip.trackIndex === other.trackIndex && clip.start === other.start &&
    clip.end === other.end && clip.itemId === other.itemId));
}

function execute(project, label, createActions) {
  return project.executeTransaction((tx) => {
    for (const action of createActions()) {
      if (!action || !tx.addAction(action)) throw new Error(`Premiere rejected ${label}.`);
    }
  }, label);
}

async function readMarks(clip, mediaType, readTicks) {
  const points = await Promise.all([clip.getInPoint(mediaType), clip.getOutPoint(mediaType)]);
  return points.map((point) => point != null && readTicks(point) >= 0n ? point : null);
}

function restoreMarks(project, clip, [originalIn, originalOut]) {
  let restored = false;
  project.lockedAccess(() => {
    restored = execute(project, "Restore Adjustment Layer Source Marks", () => {
      if (originalIn && originalOut) return [clip.createSetInOutPointsAction(originalIn, originalOut)];
      const actions = [clip.createClearInOutPointsAction()];
      if (originalIn) actions.push(clip.createSetInPointAction(originalIn));
      if (originalOut) actions.push(clip.createSetOutPointAction(originalOut));
      return actions;
    });
  });
  if (!restored) throw new Error("Premiere could not restore the adjustment layer's source In and Out points.");
}

async function placedLayer(ppro, sequence, trackIndex, identity, start, readItemRange) {
  const track = await sequence.getVideoTrack(trackIndex);
  if (!track) return null;
  const items = await track.getTrackItems(ppro.Constants.TrackItemType.CLIP, false);
  for (const item of items || []) {
    if (!item) continue;
    const [itemStart, itemEnd] = await readItemRange(item);
    if (itemStart === start && itemEnd > start && await item.isAdjustmentLayer() &&
        sourceIdentity(ppro, await item.getProjectItem()) === identity) return { item, end: itemEnd };
  }
  return null;
}

async function addAdjustmentLayer(ppro, _isCurrent, timeline) {
  const { project, sequence, projectId, sequenceId } = await activeTimeline(ppro);
  const {
    storage, findSource, readTicks, readItemRange, readSingleTrack, trackLayout, trackLayoutsEqual,
    isConnected, watchTimeline
  } = timeline;
  const readSelection = () => selectedVideoRanges(ppro, sequence, readItemRange);
  const selection = await readSelection();
  if (!selection.length) throw new Error("Select at least one video clip first.");

  const identity = storage.getItem(sourceKey(projectId));
  const source = identity && await findSource(project, (item) => sourceIdentity(ppro, item) === identity);
  if (!source?.clip) throw new Error(SETUP_MESSAGE);

  let { start, end, trackIndex: highestTrack } = selection[0];
  for (const clip of selection.slice(1)) {
    if (clip.start < start) start = clip.start;
    if (clip.end > end) end = clip.end;
    highestTrack = Math.max(highestTrack, clip.trackIndex);
  }
  const frame = BigInt(await sequence.getTimebase());
  if (frame <= 0n || start % frame !== 0n || end % frame !== 0n) {
    throw new Error("The selected range could not be matched to exact video frames.");
  }

  const editor = ppro.SequenceEditor?.getEditor(sequence);
  if (!editor) throw new Error("Premiere could not open the timeline editor.");
  const time = (ticks) => ppro.TickTime.createWithTicks(ticks.toString());
  const duration = end - start;
  const placementTime = time(start);
  const mediaType = ppro.Constants.MediaType.VIDEO;
  const sourceMarks = () => readMarks(source.clip, mediaType, readTicks);
  const lengthMatches = ([inPoint, outPoint]) => inPoint && outPoint &&
    readTicks(outPoint) - readTicks(inPoint) === duration;
  const snapshot = () => trackLayout(sequence, "Video");
  const baseline = await snapshot();
  if (highestTrack >= baseline.totalCount) throw new Error(TIMELINE_CHANGED);
  const audioTrackCount = await sequence.getAudioTrackCount();
  const sameLayout = (layout) => layout.totalCount === baseline.totalCount &&
    trackLayoutsEqual(layout.tracks, baseline.tracks);

  const withStableTimeline = async (read, commit) => {
    // Source marks can emit a delayed project change notification. Re-read
    // the actual edit inputs when a notification crosses a validation pass.
    for (let attempt = 0; attempt < 3; attempt++) {
      if (!isConnected()) throw new Error("The Premiere bridge disconnected.");
      const unchanged = watchTimeline();
      const current = await activeTimeline(ppro);
      if (current.projectId !== projectId || current.sequenceId !== sequenceId ||
          storage.getItem(sourceKey(projectId)) !== identity) {
        throw new Error(TIMELINE_CHANGED);
      }
      const inputs = await read();
      if (!unchanged()) continue;
      let ran = false;
      project.lockedAccess(() => {
        if (!unchanged()) return;
        commit(inputs);
        ran = true;
      });
      if (ran) return;
    }
    throw new Error(TIMELINE_CHANGED);
  };
  const withConfirmedTimeline = (commit) => withStableTimeline(async () => {
    if (!sameSelection(selection, await readSelection()) ||
        !sameLayout(await snapshot()) || await sequence.getAudioTrackCount() !== audioTrackCount) {
      throw new Error(TIMELINE_CHANGED);
    }
    return sourceMarks();
  }, commit);

  async function extendPlacedLayer(index, observed, trackCount) {
    const inspect = () => placedLayer(ppro, sequence, index, identity, start, readItemRange);
    try {
      await withStableTimeline(async () => {
        const current = await inspect();
        const track = await readSingleTrack(sequence, "Video", index, ppro.Constants.TrackItemType);
        if (current?.end !== observed.end || await sequence.getVideoTrackCount() !== trackCount ||
            track.locked || !track.ranges?.every(([from, to]) => to <= current.end || from >= end)) {
          throw new Error(SHORT_LAYER);
        }
        return current.item;
      }, (item) => {
        execute(project, "Extend Adjustment Layer", () => [item.createSetEndAction(time(end))]);
      });
      if ((await inspect())?.end === end) return;
    } catch (error) {
      console.warn("[Spotlight] Could not extend the placed adjustment layer:", error);
    }
    throw new Error(SHORT_LAYER);
  }

  let originalMarks;
  let restoreSource = false;
  try {
    await withConfirmedTimeline((marks) => {
      originalMarks = marks;
      if (lengthMatches(originalMarks)) return;
      restoreSource = execute(project, "Size Adjustment Layer Source", () =>
        [source.clip.createSetInOutPointsAction(time(0n), time(duration))]);
      if (!restoreSource) throw new Error("Premiere could not size the adjustment layer to the selected range.");
    });

    const candidates = baseline.tracks.flatMap(({ locked, ranges }, index) =>
      index > highestTrack && !locked && ranges?.every(([from, to]) => to <= start || from >= end)
        ? [index] : []);
    candidates.push(baseline.totalCount);

    for (const index of candidates) {
      const newTrack = index === baseline.totalCount;
      let committed = false;
      let placementError;
      await withConfirmedTimeline((marks) => {
        if (!lengthMatches(marks)) {
          throw new Error("Premiere could not match the adjustment layer to the selected duration.");
        }
        try {
          committed = execute(project, "Add Adjustment Layer", () => [newTrack
            ? editor.createInsertProjectItemAction(source.item, placementTime, index, 0, true)
            : editor.createOverwriteItemAction(source.item, placementTime, index, 0)]);
        } catch (error) {
          placementError = error;
        }
      });
      const actualTrackCount = await sequence.getVideoTrackCount();
      const observed = actualTrackCount > index
        ? await placedLayer(ppro, sequence, index, identity, start, readItemRange)
        : null;
      if (observed && observed.end <= end) {
        if (actualTrackCount !== baseline.totalCount + Number(newTrack) ||
            await sequence.getAudioTrackCount() !== audioTrackCount) {
          throw new Error("Premiere added an unexpected track. Check the timeline.");
        }
        if (observed.end < end) await extendPlacedLayer(index, observed, actualTrackCount);
        return `Added adjustment layer on V${index + 1}.`;
      }
      // A locked track can reject the edit on versions without a lock-state getter.
      // Try the next track only after proving the prior attempt left no edit.
      if ((committed && baseline.tracks.some((track) => track.ranges === null)) ||
          !sameLayout(await snapshot()) || await sequence.getAudioTrackCount() !== audioTrackCount) {
        throw new Error("Premiere did not place the layer at the expected bounds. Check the timeline.");
      }
      if (newTrack && placementError) {
        throw new Error(`Premiere could not add a new video track. ${placementError.message || placementError}`);
      }
    }
    throw new Error("Premiere could not place the adjustment layer on a new video track.");
  } finally {
    if (restoreSource) restoreMarks(project, source.clip, originalMarks);
  }
}

module.exports = { addAdjustmentLayer, registerAdjustmentLayer };
