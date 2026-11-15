"use strict";

const ppro = require("premierepro");
const { entrypoints, shell } = require("uxp");

const WS_URLS = ["ws://localhost:3847", "ws://127.0.0.1:3847"];
const HELPER_EXE_PATH = "__SPOTLIGHT_HELPER_EXE_PATH__";
let wsUrlIndex = 0;
let ws = null;
let reconnectTimer = null;
let started = false;
let cachedCatalog = [];
let failedConnects = 0;
let launchAttempted = false;
let mediaRequestId = 0;
let mediaScanGeneration = 0;
let projectContextRevision = 0;
let projectEventBindings = [];
let placementInProgress = false;

const TICKS_PER_SECOND = 254016000000n;
const DEFAULT_TICKS_PER_FRAME_30FPS = 8467200000n;

const HIDDEN_VIDEO_MATCH_NAMES = new Set([
  "AE.ADBE Motion",
  "AE.ADBE Opacity",
  "AE.ADBE Time Remapping"
]);

function setBadge(online, text) {
  const el = document.getElementById("connBadge");
  if (!el) return;
  el.textContent = text;
  if (online) {
    el.classList.add("online");
  } else {
    el.classList.remove("online");
  }
}

function setLog(msg) {
  const el = document.getElementById("statusLog");
  if (el) el.textContent = msg;
}

function setStats(msg) {
  const el = document.getElementById("catalogStats");
  if (el) el.textContent = msg;
}

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

function formatTransitionDisplayName(rawMatchName) {
  let name = String(rawMatchName || "").trim();
  // 1. Strip repeated engine prefixes: "AE.", "PR.", "ADBE ", "AE_"
  name = name.replace(/^(?:AE\.|PR\.|ADBE[\s_]+|AE[\s_]+)+/gi, "");
  // 2. Replace underscores with spaces (e.g. "Impact_Dip_To_Black" -> "Impact Dip To Black")
  name = name.replace(/_/g, " ");
  // 3. Strip internal " New" suffix used by Premiere (e.g. "Cross Dissolve New" -> "Cross Dissolve")
  name = name.replace(/\s+New$/i, "");
  // 4. Insert space in CamelCase words (e.g. "MorphCut" -> "Morph Cut", "CheckerBoard" -> "Checker Board")
  name = name.replace(/([a-z])([A-Z])/g, "$1 $2");
  // 5. Collapse multiple spaces
  name = name.replace(/\s{2,}/g, " ").trim();
  return name || String(rawMatchName || "").trim();
}

async function scanPremiereCatalog() {
  const items = [];
  const seen = new Set();

  // 1. Video Effects
  try {
    if (ppro.VideoFilterFactory) {
      const [matchNames, displayNames] = await Promise.all([
        Promise.resolve(ppro.VideoFilterFactory.getMatchNames?.() || []),
        Promise.resolve(ppro.VideoFilterFactory.getDisplayNames?.() || [])
      ]);
      const count = Math.min(matchNames.length, displayNames.length);
      for (let i = 0; i < count; i++) {
        const name = String(displayNames[i] || "").trim();
        const matchName = String(matchNames[i] || "").trim();
        if (!name || !matchName || HIDDEN_VIDEO_MATCH_NAMES.has(matchName)) continue;
        const key = `videoEffect:${matchName}`;
        if (seen.has(key)) continue;
        seen.add(key);
        items.push({
          name,
          matchName,
          effectType: "videoEffect",
          folder: "Video Effect"
        });
      }
    }
  } catch (err) {
    console.warn("[Spotlight] VideoFilterFactory scan error:", err);
  }

  // 2. Audio Effects (Premiere UXP exposes display names via getDisplayNames)
  try {
    if (ppro.AudioFilterFactory) {
      const displayNames = (await Promise.resolve(ppro.AudioFilterFactory.getDisplayNames?.())) || [];
      for (const rawName of displayNames) {
        const name = String(rawName || "").trim();
        if (!name) continue;
        const key = `audioEffect:${name}`;
        if (seen.has(key)) continue;
        seen.add(key);
        items.push({
          name,
          matchName: name,
          effectType: "audioEffect",
          folder: "Audio Effect"
        });
      }
    }
  } catch (err) {
    console.warn("[Spotlight] AudioFilterFactory scan error:", err);
  }

  // 3. Video & Audio Transitions
  try {
    if (ppro.TransitionFactory) {
      const vTrans = (await Promise.resolve(ppro.TransitionFactory.getVideoTransitionMatchNames?.())) || [];
      for (const m of vTrans) {
        const clean = String(m || "").trim();
        if (!clean) continue;
        const key = `videoTransition:${clean}`;
        if (seen.has(key)) continue;
        seen.add(key);
        items.push({
          name: formatTransitionDisplayName(clean),
          matchName: clean,
          effectType: "videoTransition",
          folder: "Video Transition"
        });
      }
    }
  } catch (err) {
    console.warn("[Spotlight] TransitionFactory scan error:", err);
  }

  cachedCatalog = items;
  setStats(`Indexed ${items.length} effects & transitions`);
  return items;
}

async function* projectEntries(project) {
  const folders = [{ folder: await project.getRootItem(), path: "" }];
  const seen = new Set();
  while (folders.length > 0) {
    const { folder, path } = folders.pop();
    for (const item of await folder.getItems()) {
      const itemId = item.getId();
      if (!itemId || seen.has(itemId)) continue;
      seen.add(itemId);
      let child;
      try {
        child = ppro.FolderItem.cast(item);
      } catch (_) {
        // Older Premiere builds throw when casting a non-bin ProjectItem.
      }
      if (typeof child?.getItems === "function") {
        folders.push({ folder: child, path: path ? `${path} / ${item.name}` : item.name });
        yield { item, itemId, path, bin: true };
        continue;
      }
      let clip;
      try {
        clip = ppro.ClipProjectItem.cast(item);
      } catch (_) {
        // Some Project Panel entries cannot be edited onto a timeline.
      }
      yield { item, itemId, path, clip: typeof clip?.isSequence === "function" ? clip : null };
    }
  }
}

function guid(object) {
  return object?.guid?.toString() || null;
}

function sendProjectItems(connection, requestId, projectId, sequenceId, items) {
  if (connection && ws === connection) {
    sendJson({ type: "project_items", requestId, projectId, sequenceId, items });
  }
}

async function syncProjectItems(connection, requestId, clear = false) {
  if (!connection || ws !== connection) return;
  const generation = ++mediaScanGeneration;
  if (clear) sendProjectItems(connection, requestId, null, null, []);
  const items = [];
  try {
    const project = await ppro.Project.getActiveProject();
    const projectId = guid(project);
    const sequence = await project?.getActiveSequence();
    const sequenceId = guid(sequence);
    if (projectId) {
      const activeItemId = (await sequence?.getProjectItem())?.getId();
      for await (const { item, clip, itemId, path, bin } of projectEntries(project)) {
        if (generation !== mediaScanGeneration || ws !== connection) return;
        let kind = bin ? "bin" : "other";
        let offline = false;
        if (itemId === activeItemId) {
          kind = "activeSequence";
        } else if (clip) {
          try {
            kind = await clip.isSequence() ? "sequence" : "media";
            if (kind === "media") offline = Boolean(await clip.isOffline?.());
          } catch (_) {
            // Keep items visible even when Premiere cannot classify their source.
          }
        }
        items.push({ itemId, name: item.name, binPath: path || "Project root", kind, offline });
      }
    }
    const currentProject = await ppro.Project.getActiveProject();
    const currentSequence = await currentProject?.getActiveSequence();
    if (generation === mediaScanGeneration) {
      if (guid(currentProject) === projectId && guid(currentSequence) === sequenceId) {
        sendProjectItems(connection, requestId, projectId, sequenceId, items);
      } else {
        sendProjectItems(connection, requestId, null, null, []);
      }
    }
  } catch (error) {
    if (generation === mediaScanGeneration) {
      sendProjectItems(connection, requestId, null, null, []);
      console.warn("[Spotlight] Project media scan failed:", error);
    }
  }
}

function bindProjectEvents() {
  const constants = ppro.Constants;
  const contextChanged = () => {
    ++projectContextRevision;
    void syncProjectItems(ws, mediaRequestId, true);
  };
  const rescanEvents = [
    constants?.ProjectEvent?.ACTIVATED ?? ppro.ProjectEvent?.EVENT_ACTIVATED,
    constants?.ProjectEvent?.CLOSED ?? ppro.ProjectEvent?.EVENT_CLOSED,
    constants?.SequenceEvent?.ACTIVATED,
    constants?.OperationCompleteEvent?.IMPORT_MEDIA_COMPLETE
  ];
  const markChanged = () => { ++projectContextRevision; };
  const timelineEvents = [
    constants?.ProjectEvent?.DIRTY ?? ppro.ProjectEvent?.EVENT_DIRTY,
    ppro.VideoTrack?.EVENT_TRACK_CHANGED,
    ppro.VideoTrack?.EVENT_TRACK_LOCK_CHANGED,
    ppro.AudioTrack?.EVENT_TRACK_CHANGED,
    ppro.AudioTrack?.EVENT_TRACK_LOCK_CHANGED
  ];
  const bindings = new Map([
    ...timelineEvents.map((event) => [event, markChanged]),
    ...rescanEvents.map((event) => [event, contextChanged])
  ]);
  for (const [event, listener] of bindings) {
    if (event === undefined || typeof ppro.EventManager?.addGlobalEventListener !== "function") continue;
    try {
      ppro.EventManager.addGlobalEventListener(event, listener, true);
      projectEventBindings.push([event, listener]);
    } catch (error) {
      console.warn("[Spotlight] Could not subscribe to project event:", error);
    }
  }
}

function readTicks(time) {
  if (typeof time?.ticks === "bigint") return time.ticks;
  if (typeof time?.ticks === "string" && /^-?\d+$/.test(time.ticks)) return BigInt(time.ticks);
  if (Number.isSafeInteger(time?.ticks)) return BigInt(time.ticks);
  throw new Error("Premiere could not provide a valid timeline time.");
}

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
  const audioOnly = /\.(?:wav|wave|bwf|aif|aiff|aifc|flac|mp3|aac)$/i.test(path || item.name);
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

async function trackLayout(sequence, kind) {
  const count = await sequence[`get${kind}TrackCount`]();
  if (!Number.isSafeInteger(count) || count < 0) {
    throw new Error("Premiere could not verify the destination tracks.");
  }
  const trackItemType = ppro.Constants.TrackItemType;
  const tracks = [];
  for (let index = 0; index < count; index++) {
    const track = await sequence[`get${kind}Track`](index);
    if (!track) throw new Error("Premiere could not verify the destination tracks.");
    let locked = false;
    if (typeof track.isLocked === "function") locked = await track.isLocked();
    let ranges = null;
    try {
      const clips = (await track.getTrackItems(trackItemType.CLIP, false)) ?? [];
      const transitions = (await track.getTrackItems(trackItemType.TRANSITION, false)) ?? [];
      if (Array.isArray(clips) && Array.isArray(transitions)) {
        const items = [...clips, ...transitions].filter((item) => item != null);
        ranges = await Promise.all(items.map(async (item) => {
          const start = readTicks(await item.getStartTime());
          let end;
          try {
            end = readTicks(await item.getEndTime());
          } catch (_) {
            end = start + readTicks(await item.getDuration());
          }
          if (end < start) throw new Error("Unverified track item span");
          return [start, end];
        }));
        ranges = ranges.filter(([start, end]) => end > start);
      }
    } catch (_) {
      // Treat a track with unreadable clip or transition bounds as occupied.
    }
    tracks.push({ id: track.id, locked, ranges });
  }
  return tracks;
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

// Premiere may route the edit to a different track. Verify the new clip before reporting its lane.
async function placedAudioTracks(sequence, itemId, start, frame, previousTracks) {
  const tracks = [];
  const count = await sequence.getAudioTrackCount();
  for (let index = 0; index < count; index++) {
    const previous = previousTracks[index]?.ranges;
    if (previous === null) continue;
    let items;
    try {
      const track = await sequence.getAudioTrack(index);
      items = await track.getTrackItems(ppro.Constants.TrackItemType.CLIP, false);
    } catch (_) {
      continue;
    }
    if (!Array.isArray(items)) continue;
    for (const item of items) {
      try {
        const itemStart = readTicks(await item.getStartTime());
        if (itemStart <= start - frame || itemStart >= start + frame) continue;
        const itemEnd = readTicks(await item.getEndTime());
        if (previous?.some(([beforeStart, beforeEnd]) =>
          beforeStart === itemStart && beforeEnd === itemEnd)) continue;
        if ((await item.getProjectItem())?.getId() !== itemId) continue;
        tracks.push(`A${index + 1}`);
        break;
      } catch (_) {
        // This clip cannot verify the placement.
      }
    }
  }
  return tracks;
}

async function placeProjectItem(payload, connection) {
  const revision = projectContextRevision;
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

  let source;
  for await (const candidate of projectEntries(project)) {
    if (candidate.itemId === payload.itemId) {
      source = candidate;
      break;
    }
  }
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
  const snapshot = () => Promise.all([trackLayout(sequence, "Video"), trackLayout(sequence, "Audio")]);
  const trials = await snapshot();
  const confirmedFootprint = await sourceFootprint(source, nested);
  const latest = await snapshot();
  if (await source.clip.isOffline?.()) throw new Error("Relink the offline media before placing it.");
  const latestProject = await ppro.Project.getActiveProject();
  const latestSequence = await latestProject?.getActiveSequence();
  const latestPlayhead = await latestSequence?.getPlayerPosition();
  const signature = (layout) => JSON.stringify(layout, (_, value) =>
    typeof value === "bigint" ? value.toString() : value);
  if (guid(latestProject) !== payload.projectId || guid(latestSequence) !== sequenceId ||
      readTicks(latestPlayhead) !== start || signature(trials) !== signature(latest) ||
      footprint.video !== confirmedFootprint.video || footprint.audio !== confirmedFootprint.audio) {
    throw new Error("Timeline changed before placement. Try again.");
  }
  const [videoTracks, audioTracks] = latest;
  const longest = footprint.video === null || footprint.audio === null ? null :
    (footprint.video > footprint.audio ? footprint.video : footprint.audio);
  const videoSpan = footprint.video === 0n ? 0n : longest;
  const audioSpan = footprint.audio === 0n ? 0n : longest;
  const video = firstSafeTrack(videoTracks, protectedStart, protectedDuration(videoSpan), Boolean(nested));
  const audio = firstSafeTrack(
    audioTracks, protectedStart, protectedDuration(audioSpan), Boolean(nested)
  );
  const usesNewTrack = (footprint.video !== 0n && video === videoTracks.length) ||
    (footprint.audio !== 0n && audio === audioTracks.length);
  let committed = false;
  project.lockedAccess(() => {
    if (revision !== projectContextRevision || ws !== connection || connection.readyState !== WebSocket.OPEN) {
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

async function openInSourceMonitor(payload) {
  const monitor = ppro.SourceMonitor ?? ppro.sourceMonitor;
  if (typeof monitor?.openProjectItem !== "function") {
    throw new Error("Opening in the Source Monitor requires Premiere Pro 25.6 or later.");
  }
  const project = await ppro.Project.getActiveProject();
  if (!project || guid(project) !== payload.projectId) {
    throw new Error("Active project changed. Search again.");
  }
  let source;
  for await (const candidate of projectEntries(project)) {
    if (candidate.itemId === payload.itemId) {
      source = candidate;
      break;
    }
  }
  if (!source) throw new Error("Project item was removed. Search again.");
  if (source.bin) throw new Error("Bins can't open in the Source Monitor.");
  if (!source.clip) throw new Error("This project item can't open in the Source Monitor.");
  if (!(await monitor.openProjectItem(source.item))) {
    throw new Error(`Premiere could not open "${source.item.name}" in the Source Monitor.`);
  }
  return `Opened "${source.item.name}" in the Source Monitor.`;
}

function resolveEffectEntry(payload) {
  const reqName = String(payload.name || "").trim();
  const reqMatch = String(payload.matchName || "").trim();
  const reqType = String(payload.effectType || "videoEffect").trim();

  if (cachedCatalog.length > 0) {
    const exactMatch = cachedCatalog.find(
      (item) => item.effectType === reqType && (item.matchName === reqMatch || item.name === reqName)
    );
    if (exactMatch) return exactMatch;

    const lowerName = reqName.toLowerCase();
    const lowerMatch = reqMatch.toLowerCase();
    const ciMatch = cachedCatalog.find(
      (item) =>
        item.effectType === reqType &&
        (item.name.toLowerCase() === lowerName || item.matchName.toLowerCase() === lowerMatch)
    );
    if (ciMatch) return ciMatch;
  }

  return {
    name: reqName || reqMatch,
    matchName: reqMatch || reqName,
    effectType: reqType
  };
}

function createTickTimeFromTicks(ticksBigInt) {
  const safeTicks = ticksBigInt > 0n ? ticksBigInt : DEFAULT_TICKS_PER_FRAME_30FPS;
  try {
    if (typeof ppro.TickTime?.createWithTicks === "function") {
      return ppro.TickTime.createWithTicks(String(safeTicks));
    }
  } catch (_) {}
  try {
    const seconds = Number(safeTicks) / Number(TICKS_PER_SECOND);
    if (typeof ppro.TickTime?.createWithSeconds === "function") {
      return ppro.TickTime.createWithSeconds(seconds);
    }
    if (typeof ppro.TickTime?.fromSeconds === "function") {
      return ppro.TickTime.fromSeconds(seconds);
    }
  } catch (_) {}
  return undefined;
}

async function getSequenceTicksPerFrame(sequence) {
  // 1. Check sequence.getTimebase() (in Premiere, timebase is ticks per frame as a string/number)
  try {
    const tb = await Promise.resolve(sequence.getTimebase?.());
    if (tb !== undefined && tb !== null) {
      const num = Number(tb);
      if (Number.isFinite(num) && num > 0) {
        if (num > 100000) {
          return BigInt(Math.round(num));
        }
        // If returned as FPS (e.g., 23.976, 24, 29.97, 30, 60)
        return BigInt(Math.round(Number(TICKS_PER_SECOND) / num));
      }
    }
  } catch (_) {}

  // 2. Check sequence.getSettings() -> getVideoFrameRate()
  try {
    const settings = await Promise.resolve(sequence.getSettings?.());
    const fr = await Promise.resolve(settings?.getVideoFrameRate?.());
    if (fr?.ticks !== undefined) {
      const t = BigInt(fr.ticks);
      if (t > 0n) return t;
    }
    if (fr?.seconds !== undefined && Number(fr.seconds) > 0) {
      return BigInt(Math.round(Number(fr.seconds) * Number(TICKS_PER_SECOND)));
    }
  } catch (_) {}

  return DEFAULT_TICKS_PER_FRAME_30FPS;
}

async function computeTransitionDurationInfo(sequence, prefDuration, fallbackFrames) {
  const ticksPerFrame = await getSequenceTicksPerFrame(sequence);
  let durationTicks = BigInt(fallbackFrames) * ticksPerFrame;
  let label = `${fallbackFrames}f`;

  if (prefDuration && typeof prefDuration.value === "number" && prefDuration.value > 0) {
    const val = prefDuration.value;
    const unit = String(prefDuration.unit || "frames").toLowerCase();
    if (unit === "seconds") {
      durationTicks = BigInt(Math.round(val * Number(TICKS_PER_SECOND)));
      label = `${val}s`;
    } else {
      const frames = Math.max(1, Math.round(val));
      durationTicks = BigInt(frames) * ticksPerFrame;
      label = `${frames}f`;
    }
  }

  return {
    durationTicks,
    durationTickTime: createTickTimeFromTicks(durationTicks),
    ticksPerFrame,
    label
  };
}

function createTransitionOptions({
  applyToStart,
  forceSingleSided = false,
  alignment = 0.5,
  durationTickTime
}) {
  if (typeof ppro.AddTransitionOptions !== "function") {
    return undefined;
  }
  let opts;
  try {
    opts = new ppro.AddTransitionOptions();
  } catch (_) {
    opts = ppro.AddTransitionOptions();
  }
  if (!opts) return undefined;
  try {
    opts.setApplyToStart?.(Boolean(applyToStart));
  } catch (_) {}
  try {
    opts.setForceSingleSided?.(Boolean(forceSingleSided));
  } catch (_) {}
  if (typeof alignment === "number") {
    try {
      opts.setTransitionAlignment?.(alignment);
    } catch (_) {}
  }
  if (durationTickTime) {
    try {
      opts.setDuration?.(durationTickTime);
    } catch (_) {}
  }
  return opts;
}

async function planTrackTransitionTargets(sequence, trackClipsList) {
  const metas = await Promise.all(
    trackClipsList.map(async (clip) => {
      const [startTime, endTime, inPoint, outPoint, trackIndex] = await Promise.all([
        Promise.resolve(clip.getStartTime?.()),
        Promise.resolve(clip.getEndTime?.()),
        Promise.resolve(clip.getInPoint?.()),
        Promise.resolve(clip.getOutPoint?.()),
        Promise.resolve(clip.getTrackIndex?.())
      ]);
      const startTicks = startTime?.ticks !== undefined ? BigInt(startTime.ticks) : 0n;
      const endTicks = endTime?.ticks !== undefined ? BigInt(endTime.ticks) : 0n;
      const inTicks = inPoint?.ticks !== undefined ? BigInt(inPoint.ticks) : 0n;
      const clipSpan = endTicks > startTicks ? endTicks - startTicks : 0n;
      const outTicks =
        outPoint?.ticks !== undefined ? BigInt(outPoint.ticks) : inTicks + clipSpan;
      const track = typeof trackIndex === "number" ? trackIndex : 0;
      return { clip, startTicks, endTicks, inTicks, outTicks, track };
    })
  );

  // Group selected clips by track index
  const byTrack = new Map();
  for (const item of metas) {
    if (!byTrack.has(item.track)) {
      byTrack.set(item.track, []);
    }
    byTrack.get(item.track).push(item);
  }

  let playheadTicks = null;
  try {
    const pos = await sequence.getPlayerPosition?.();
    if (pos?.ticks !== undefined) {
      playheadTicks = BigInt(pos.ticks);
    }
  } catch (_) {}

  const ONE_FRAME_TOLERANCE = 12000000000n; // ~1 frame in Premiere ticks
  const targets = [];

  for (const trackItems of byTrack.values()) {
    trackItems.sort((a, b) => (a.startTicks < b.startTicks ? -1 : a.startTicks > b.startTicks ? 1 : 0));

    if (trackItems.length >= 2) {
      // Multi-clip selection on this track: apply transitions ONLY at the inner cuts/boundaries between consecutive clips
      for (let i = 0; i < trackItems.length - 1; i++) {
        const current = trackItems[i];
        const next = trackItems[i + 1];
        const gap = next.startTicks - current.endTicks;

        if (gap >= -ONE_FRAME_TOLERANCE && gap <= ONE_FRAME_TOLERANCE) {
          // Adjacent cut: place transition across the cut between current and next
          targets.push({
            kind: "cut",
            clip: current.clip,
            clipMeta: current,
            applyToStart: false,
            forceSingleSided: false,
            alignment: 0.5,
            fallbackClip: next.clip,
            fallbackMeta: next
          });
        } else {
          // Non-adjacent clips with a gap: apply to the two inner facing edges (current End + next Start)
          targets.push({
            kind: "edge",
            clip: current.clip,
            clipMeta: current,
            applyToStart: false,
            forceSingleSided: true,
            alignment: 1.0,
            fallbackClip: null,
            fallbackMeta: null
          });
          targets.push({
            kind: "edge",
            clip: next.clip,
            clipMeta: next,
            applyToStart: true,
            forceSingleSided: true,
            alignment: 0.0,
            fallbackClip: null,
            fallbackMeta: null
          });
        }
      }
    } else if (trackItems.length === 1) {
      // Single-clip selection on this track: pick start vs end based on Playhead proximity
      const single = trackItems[0];
      let applyToStart = false;
      if (playheadTicks !== null) {
        const distStart =
          playheadTicks >= single.startTicks
            ? playheadTicks - single.startTicks
            : single.startTicks - playheadTicks;
        const distEnd =
          playheadTicks >= single.endTicks
            ? playheadTicks - single.endTicks
            : single.endTicks - playheadTicks;
        applyToStart = distStart <= distEnd;
      }
      targets.push({
        kind: "single",
        clip: single.clip,
        clipMeta: single,
        applyToStart,
        forceSingleSided: false,
        alignment: 0.5,
        fallbackClip: null,
        fallbackMeta: null
      });
    }
  }

  return targets;
}

async function applyEffectToSelection(payload) {
  if (cachedCatalog.length === 0) {
    await scanPremiereCatalog();
  }

  const resolved = resolveEffectEntry(payload);
  const { name, matchName, effectType } = resolved;

  const project = await ppro.Project.getActiveProject();
  if (!project) {
    throw new Error("Open a Premiere Pro project first.");
  }

  const sequence = await project.getActiveSequence();
  if (!sequence) {
    throw new Error("Open a sequence in the Timeline first.");
  }

  const selection = await sequence.getSelection();
  if (!selection) {
    throw new Error("Select a clip in the Timeline first.");
  }

  const clips = (await selection.getTrackItems()) || [];
  if (clips.length === 0) {
    throw new Error("Select at least one timeline clip first.");
  }

  if (effectType === "videoEffect") {
    const videoClips = clips.filter((clip) => isVideoClip(clip));
    if (videoClips.length === 0) {
      throw new Error(`Select a video clip before applying "${name}".`);
    }

    const prepared = await Promise.all(
      videoClips.map(async (clip) => {
        const [chain, component] = await Promise.all([
          clip.getComponentChain(),
          ppro.VideoFilterFactory.createComponent(matchName)
        ]);
        if (!chain || !component) {
          throw new Error(`Premiere could not create video effect "${name}" (${matchName}).`);
        }
        return { chain, component };
      })
    );

    let ok = false;
    project.lockedAccess(() => {
      ok = project.executeTransaction((tx) => {
        for (const { chain, component } of prepared) {
          tx.addAction(chain.createAppendComponentAction(component));
        }
      }, `Apply ${name}`);
    });

    if (!ok) {
      throw new Error(`Premiere rejected applying "${name}".`);
    }
    return `Applied "${name}" to ${prepared.length} video clip${prepared.length === 1 ? "" : "s"}.`;
  }

  if (effectType === "audioEffect") {
    const audioClips = clips.filter((clip) => isAudioClip(clip));
    if (audioClips.length === 0) {
      throw new Error(`Select an audio clip before applying "${name}".`);
    }

    const prepared = await Promise.all(
      audioClips.map(async (clip) => {
        const chain = await clip.getComponentChain();
        let component = null;
        if (typeof ppro.AudioFilterFactory.createComponentByDisplayName === "function") {
          component = await ppro.AudioFilterFactory.createComponentByDisplayName(name, clip);
        } else if (typeof ppro.AudioFilterFactory.createComponent === "function") {
          component = await ppro.AudioFilterFactory.createComponent(matchName || name);
        }
        if (!chain || !component) {
          throw new Error(`Premiere could not create audio effect "${name}".`);
        }
        return { chain, component };
      })
    );

    let ok = false;
    project.lockedAccess(() => {
      ok = project.executeTransaction((tx) => {
        for (const { chain, component } of prepared) {
          tx.addAction(chain.createAppendComponentAction(component));
        }
      }, `Apply ${name}`);
    });

    if (!ok) {
      throw new Error(`Premiere rejected applying "${name}".`);
    }
    return `Applied "${name}" to ${prepared.length} audio clip${prepared.length === 1 ? "" : "s"}.`;
  }

  if (effectType === "videoTransition" && ppro.TransitionFactory) {
    const videoClips = clips.filter((clip) => isVideoClip(clip));
    if (videoClips.length === 0) {
      throw new Error(`Select a video clip before applying "${name}".`);
    }

    const [targets, durationInfo] = await Promise.all([
      planTrackTransitionTargets(sequence, videoClips),
      computeTransitionDurationInfo(sequence, payload.videoTransitionDuration, 30)
    ]);

    if (targets.length === 0) {
      throw new Error(`No valid transition cut points found for "${name}".`);
    }

    const prepared = await Promise.all(
      targets.map(async (target) => {
        const transition = await ppro.TransitionFactory.createVideoTransition(matchName);
        const fallbackTransition = target.fallbackClip
          ? await ppro.TransitionFactory.createVideoTransition(matchName)
          : null;
        return {
          ...target,
          transition,
          fallbackTransition
        };
      })
    );

    let applied = 0;
    project.lockedAccess(() => {
      for (const item of prepared) {
        if (!item.transition || typeof item.clip.createAddVideoTransitionAction !== "function") {
          continue;
        }
        const opts = createTransitionOptions({
          applyToStart: item.applyToStart,
          forceSingleSided: item.forceSingleSided,
          alignment: item.alignment,
          durationTickTime: durationInfo.durationTickTime
        });

        let committed = project.executeTransaction((tx) => {
          const action = opts
            ? item.clip.createAddVideoTransitionAction(item.transition, opts)
            : item.clip.createAddVideoTransitionAction(item.transition);
          if (action) {
            tx.addAction(action);
          }
        }, `Apply ${name}`);

        // If applying to outgoing clip end was rejected (e.g. insufficient tail handle), try incoming clip start at the same cut
        if (!committed && item.fallbackClip && item.fallbackTransition) {
          const fallbackOpts = createTransitionOptions({
            applyToStart: true,
            forceSingleSided: false,
            alignment: 0.5,
            durationTickTime: durationInfo.durationTickTime
          });
          committed = project.executeTransaction((tx) => {
            const action = fallbackOpts
              ? item.fallbackClip.createAddVideoTransitionAction(item.fallbackTransition, fallbackOpts)
              : item.fallbackClip.createAddVideoTransitionAction(item.fallbackTransition);
            if (action) {
              tx.addAction(action);
            }
          }, `Apply ${name}`);
        }

        if (committed) {
          applied++;
        }
      }
    });

    if (applied === 0) {
      throw new Error(`Could not apply transition "${name}".`);
    }
    return `Applied "${name}" (${durationInfo.label}) to ${applied} cut${applied === 1 ? "" : "s"}.`;
  }

  throw new Error(`Unsupported effect type: ${effectType}`);
}

function sendJson(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(obj));
  }
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connectBridge();
  }, 1200);
}

function connectBridge() {
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }

  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
    return;
  }

  const url = WS_URLS[wsUrlIndex % WS_URLS.length];
  wsUrlIndex++;

  let connection;
  try {
    connection = new WebSocket(url);
  } catch (err) {
    setBadge(false, "Offline");
    setLog(`Connection error (${url}): ${err && err.message ? err.message : String(err)}`);
    scheduleReconnect();
    return;
  }

  ws = connection;

  connection.onopen = async () => {
    if (ws !== connection) return;
    failedConnects = 0;
    launchAttempted = true;
    sendJson({ type: "ping" });
    mediaRequestId = 0;
    void syncProjectItems(connection, mediaRequestId, true);
    setBadge(true, "Connected");
    setLog(`Connected to Spotlight helper (${url}).`);
    try {
      const items = await scanPremiereCatalog();
      if (items.length > 0 && ws === connection && connection.readyState === WebSocket.OPEN) {
        sendJson({ type: "catalog", effects: items });
      }
    } catch (err) {
      console.warn("[Spotlight] Catalog scan on open failed:", err);
    }
  };

  connection.onmessage = async (event) => {
    if (ws !== connection || typeof event.data !== "string") return;
    try {
      const msg = JSON.parse(event.data);
      if (msg.type === "request_project_items" && Number.isSafeInteger(msg.requestId)) {
        mediaRequestId = msg.requestId;
        void syncProjectItems(connection, mediaRequestId);
      } else if (msg.type === "place_project_item" && Number.isSafeInteger(msg.requestId)) {
        let ok = false;
        let message;
        if (placementInProgress) {
          message = "A media placement is already in progress.";
        } else {
          placementInProgress = true;
          try {
            message = await placeProjectItem(msg, connection);
            ok = true;
          } catch (error) {
            message = error.message || String(error);
          } finally {
            placementInProgress = false;
          }
        }
        if (ws === connection) {
          setLog(message);
          sendJson({ type: "placement_ack", requestId: msg.requestId, ok, message });
        }
      } else if (msg.type === "open_in_source_monitor" && Number.isSafeInteger(msg.requestId)) {
        let ok = false;
        let message;
        try {
          message = await openInSourceMonitor(msg);
          ok = true;
        } catch (error) {
          message = error.message || String(error);
        }
        if (ws === connection) {
          setLog(message);
          sendJson({ type: "source_monitor_ack", requestId: msg.requestId, ok, message });
        }
      } else if (msg.type === "apply_effect") {
        setLog(`Applying ${msg.name}...`);
        try {
          const resultMsg = await applyEffectToSelection(msg);
          setLog(resultMsg);
          if (ws === connection) sendJson({ type: "ack", requestId: msg.requestId, ok: true, message: resultMsg });
        } catch (err) {
          const errMsg = err && err.message ? err.message : String(err);
          setLog(`Error: ${errMsg}`);
          if (ws === connection) sendJson({ type: "ack", requestId: msg.requestId, ok: false, message: errMsg });
        }
      } else if (msg.type === "request_catalog") {
        const items = await scanPremiereCatalog();
        sendJson({ type: "catalog", effects: items });
      }
    } catch (e) {
      console.error("[Spotlight] Failed to handle WS message:", e);
    }
  };

  connection.onclose = () => {
    if (ws !== connection) return;
    ws = null;
    failedConnects++;
    if (failedConnects >= 2 && !launchAttempted) {
      launchAttempted = true;
      try {
        if (shell && typeof shell.openPath === "function" && !HELPER_EXE_PATH.startsWith("__SPOTLIGHT_")) {
          shell.openPath(HELPER_EXE_PATH);
        }
      } catch (_) {}
    }
    setBadge(false, "Reconnecting...");
    scheduleReconnect();
  };

  connection.onerror = () => {
    if (ws !== connection) return;
    try {
      connection.close();
    } catch (_) {}
  };
}

let uiBound = false;
function bindPanelUI() {
  const btn = document.getElementById("rescanBtn");
  if (!btn || uiBound) return;
  uiBound = true;
  btn.addEventListener("click", async () => {
    setLog("Scanning Premiere Pro effects...");
    void syncProjectItems(ws, mediaRequestId);
    const items = await scanPremiereCatalog();
    if (ws && ws.readyState === WebSocket.OPEN) {
      sendJson({ type: "catalog", effects: items });
      setLog(`Rescanned and synced ${items.length} effects.`);
    } else {
      setLog(`Scanned ${items.length} effects. Reconnecting to helper...`);
      connectBridge();
    }
  });
}

async function startPlugin() {
  bindPanelUI();
  if (started) {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
      connectBridge();
    }
    return;
  }
  started = true;
  bindProjectEvents();
  try {
    await scanPremiereCatalog();
  } catch (_) {}
  if (started) connectBridge();
}

function stopPlugin() {
  started = false;
  ++mediaScanGeneration;
  ++projectContextRevision;
  for (const [event, listener] of projectEventBindings) {
    try {
      ppro.EventManager.removeGlobalEventListener?.(event, listener, true);
    } catch (error) {
      console.warn("[Spotlight] Could not remove project event listener:", error);
    }
  }
  projectEventBindings = [];
  if (reconnectTimer) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (ws) {
    try {
      ws.close();
    } catch (_) {}
    ws = null;
  }
}

try {
  entrypoints.setup({
    plugin: {
      create: startPlugin,
      destroy: stopPlugin
    },
    panels: {
      premiereSpotlightBridgePanel: {
        create: startPlugin,
        show: startPlugin
      }
    }
  });
} catch (err) {
  console.warn("[Spotlight] entrypoints.setup fallback:", err);
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", startPlugin);
} else {
  startPlugin();
}
