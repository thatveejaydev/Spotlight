"use strict";

// Timeline time and track reading shared by placement, effects and actions.

const { mapWithConcurrency } = require("./util.js");

const TICKS_PER_SECOND = 254016000000n;
const DEFAULT_TICKS_PER_FRAME_30FPS = 8467200000n;
const TRACK_ITEM_READ_CONCURRENCY = 32;

module.exports = function createTimeline(ppro) {
  function isVideoClip(clip) {
    if (ppro.VideoClipTrackItem) {
      return clip instanceof ppro.VideoClipTrackItem;
    }
    return true;
  }

  function isAudioClip(clip) {
    if (ppro.AudioClipTrackItem) {
      return clip instanceof ppro.AudioClipTrackItem;
    }
    return true;
  }

  function readTicks(time) {
    if (typeof time?.ticks === "bigint") return time.ticks;
    if (typeof time?.ticks === "string" && /^-?\d+$/.test(time.ticks)) return BigInt(time.ticks);
    if (Number.isSafeInteger(time?.ticks)) return BigInt(time.ticks);
    throw new Error("Premiere could not provide a valid timeline time.");
  }

  async function readItemRange(item) {
    const [startTime, endTime] = await Promise.all([
      Promise.resolve().then(() => item.getStartTime()),
      Promise.resolve().then(() => item.getEndTime()).catch(() => null)
    ]);
    const start = readTicks(startTime);
    const end = endTime !== null
      ? readTicks(endTime)
      : start + readTicks(await item.getDuration());
    if (end < start) throw new Error("Unverified track item span");
    return [start, end];
  }

  async function readSingleTrack(sequence, kind, index, trackItemType) {
    const track = await sequence[`get${kind}Track`](index);
    if (!track) throw new Error("Premiere could not verify the destination tracks.");
    let locked = false;
    if (typeof track.isLocked === "function") locked = await track.isLocked();
    if (locked) return { id: track.id, locked: true, ranges: [] };
    let ranges = null;
    try {
      const [clipsRaw, transitionsRaw] = await Promise.all([
        track.getTrackItems(trackItemType.CLIP, false),
        track.getTrackItems(trackItemType.TRANSITION, false)
      ]);
      const clips = clipsRaw ?? [];
      const transitions = transitionsRaw ?? [];
      if (Array.isArray(clips) && Array.isArray(transitions)) {
        const items = [...clips, ...transitions].filter((item) => item != null);
        ranges = await mapWithConcurrency(items, TRACK_ITEM_READ_CONCURRENCY, readItemRange);
        ranges = ranges.filter(([start, end]) => end > start);
      }
    } catch (_) {
      // Treat a track with unreadable clip or transition bounds as occupied.
    }
    return { id: track.id, locked, ranges };
  }

  async function trackLayout(sequence, kind, limit = null) {
    const totalCount = await sequence[`get${kind}TrackCount`]();
    if (!Number.isSafeInteger(totalCount) || totalCount < 0) {
      throw new Error("Premiere could not verify the destination tracks.");
    }
    const scanCount = limit === null ? totalCount : Math.min(totalCount, limit);
    const trackItemType = ppro.Constants.TrackItemType;
    const tracks = await Promise.all(
      Array.from({ length: scanCount }, (_, index) => readSingleTrack(sequence, kind, index, trackItemType))
    );
    return { tracks, totalCount };
  }

  function trackLayoutsEqual(a, b) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      const ta = a[i];
      const tb = b[i];
      if (ta.id !== tb.id || ta.locked !== tb.locked) return false;
      if (ta.ranges === null || tb.ranges === null) {
        if (ta.ranges !== tb.ranges) return false;
        continue;
      }
      if (ta.ranges.length !== tb.ranges.length) return false;
      for (let j = 0; j < ta.ranges.length; j++) {
        if (ta.ranges[j][0] !== tb.ranges[j][0] || ta.ranges[j][1] !== tb.ranges[j][1]) {
          return false;
        }
      }
    }
    return true;
  }

  function firstSafeTrack(tracks, start, duration, maySpanTracks) {
    if (duration === 0n) return 0;
    // If the duration is unknown, only an open-ended gap is safe.
    const end = duration === null ? null : start + duration;
    const hasRoom = (track) => !track.locked && track.ranges !== null &&
      track.ranges.every(([itemStart, itemEnd]) => itemEnd <= start || (end !== null && end <= itemStart));
    // A nested sequence can affect downstream tracks with unknown routing.
    const index = tracks.findIndex((track, index) => hasRoom(track) &&
      (!maySpanTracks || tracks.slice(index + 1).every(hasRoom)));
    return index === -1 ? tracks.length : index;
  }

  return {
    TICKS_PER_SECOND,
    DEFAULT_TICKS_PER_FRAME_30FPS,
    TRACK_ITEM_READ_CONCURRENCY,
    isVideoClip,
    isAudioClip,
    readTicks,
    readItemRange,
    readSingleTrack,
    trackLayout,
    trackLayoutsEqual,
    firstSafeTrack
  };
};
