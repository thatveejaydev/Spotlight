"use strict";

// Premiere Spotlight bridge: connects to the helper over WebSocket, keeps the
// effect and project-item caches, and routes helper commands to the modules in
// ./lib and ./actions. The modules hold no state; everything mutable lives here.

const ppro = require("premierepro");
const { runAction } = require("./actions/index.js");
const { runMarkerCommand } = require("./actions/markers.js");
const { entrypoints, shell } = require("uxp");
const { nowMs, mapWithConcurrency, guid } = require("./lib/util.js");
const {
  readTicks,
  readItemRange,
  readSingleTrack,
  trackLayout,
  trackLayoutsEqual
} = require("./lib/timeline.js")(ppro);
const {
  PROJECT_ROOT_BIN,
  ITEM_SCAN_CONCURRENCY,
  collectProjectEntries
} = require("./lib/project-items.js")(ppro);
const effects = require("./lib/effects.js")(ppro);
const placement = require("./lib/placement.js")(ppro, { findProjectEntry });
const sourceMedia = require("./lib/source-media.js")(ppro, { findProjectEntry });

const WS_URLS = ["ws://localhost:3847", "ws://127.0.0.1:3847"];
const HELPER_EXE_PATH = "__SPOTLIGHT_HELPER_EXE_PATH__";
let wsUrlIndex = 0;
let ws = null;
let reconnectTimer = null;
let started = false;
let cachedCatalog = [];
let catalogScanPromise = null;
let cachedCatalogStatsText = "";
let cachedProjectEntries = { projectId: null, byId: new Map() };
let failedConnects = 0;
let launchAttempted = false;
let mediaRequestId = 0;
let mediaScanGeneration = 0;
let projectContextRevision = 0;
// Last list sent to the helper, reused while projectContextRevision is unchanged.
let lastProjectScan = null;
let projectEventBindings = [];
let placementInProgress = false;


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

async function scanPremiereCatalogInternal() {
  const { items, elapsedMs } = await effects.scanEffectCatalog();
  cachedCatalog = items;
  cachedCatalogStatsText = `Indexed ${items.length} effects & transitions (${elapsedMs} ms)`;
  setStats(cachedCatalogStatsText);
  return items;
}

async function scanPremiereCatalog() {
  if (catalogScanPromise) return catalogScanPromise;
  catalogScanPromise = (async () => {
    try {
      return await scanPremiereCatalogInternal();
    } finally {
      catalogScanPromise = null;
    }
  })();
  return catalogScanPromise;
}

async function ensureCatalogScanned(force = false) {
  if (!force && cachedCatalog.length > 0) return cachedCatalog;
  return scanPremiereCatalog();
}

async function findProjectEntry(project, projectId, itemId) {
  if (cachedProjectEntries.projectId === projectId) {
    const cached = cachedProjectEntries.byId.get(itemId);
    if (cached && cached.item?.getId?.() === itemId) {
      return cached;
    }
  }
  const result = await collectProjectEntries(project, { targetItemId: itemId });
  return result?.found || null;
}

function sendProjectItems(connection, requestId, projectId, sequenceId, items, scanMode = "cleared", scanMs = 0) {
  if (connection && ws === connection) {
    sendJson({ type: "project_items", requestId, projectId, sequenceId, items, scanMode, scanMs });
  }
}

// `reuse` sends the last list without walking the project when nothing changed since it was built.
async function syncProjectItems(connection, requestId, clear = false, reuse = false) {
  if (!connection || ws !== connection) return;
  const generation = ++mediaScanGeneration;
  const revision = projectContextRevision;
  const startedAt = nowMs();
  const stale = () => generation !== mediaScanGeneration || ws !== connection;
  if (clear) {
    lastProjectScan = null;
    cachedProjectEntries = { projectId: null, byId: new Map() };
    sendProjectItems(connection, requestId, null, null, [], "scanning");
  }
  try {
    const project = await ppro.Project.getActiveProject();
    const projectId = guid(project);
    const sequence = await project?.getActiveSequence();
    const sequenceId = guid(sequence);
    if (stale()) return;

    const previous = lastProjectScan;
    if (reuse && previous && previous.revision === revision && revision === projectContextRevision
        && previous.projectId === projectId && previous.sequenceId === sequenceId) {
      const elapsedMs = Math.max(0, Math.round(nowMs() - startedAt));
      sendProjectItems(connection, requestId, projectId, sequenceId, previous.items, "reused", elapsedMs);
      return;
    }

    let items = [];
    let nextById = new Map();
    if (projectId) {
      const [activeProjectItem, collected] = await Promise.all([
        Promise.resolve(sequence?.getProjectItem?.()),
        collectProjectEntries(project, { shouldAbort: stale })
      ]);
      if (stale() || !collected) return;
      const activeItemId = activeProjectItem?.getId?.();
      nextById = collected.byId;
      items = await mapWithConcurrency(
        collected.entries,
        ITEM_SCAN_CONCURRENCY,
        async ({ item, clip, itemId, path, bin }) => {
          let kind = bin ? "bin" : "other";
          let offline = false;
          if (itemId === activeItemId) {
            kind = "activeSequence";
          } else if (clip && !stale()) {
            try {
              kind = (await clip.isSequence()) ? "sequence" : "media";
              if (kind === "media" && !stale()) {
                offline = Boolean(await clip.isOffline?.());
              }
            } catch (_) {
              // Keep items visible even when Premiere cannot classify their source.
            }
          }
          return { itemId, name: item.name, binPath: path || PROJECT_ROOT_BIN, kind, offline };
        }
      );
      if (stale()) return;
    }

    const currentProject = await ppro.Project.getActiveProject();
    const currentSequence = await currentProject?.getActiveSequence();
    if (generation === mediaScanGeneration) {
      if (guid(currentProject) === projectId && guid(currentSequence) === sequenceId) {
        cachedProjectEntries = { projectId, byId: nextById };
        lastProjectScan = { revision, projectId, sequenceId, items };
        const elapsedMs = Math.max(0, Math.round(nowMs() - startedAt));
        sendProjectItems(connection, requestId, projectId, sequenceId, items, "rescanned", elapsedMs);
        if (cachedCatalogStatsText) {
          setStats(`${cachedCatalogStatsText} • ${items.length} media (${elapsedMs} ms)`);
        }
      } else {
        lastProjectScan = null;
        cachedProjectEntries = { projectId: null, byId: new Map() };
        sendProjectItems(connection, requestId, null, null, []);
      }
    }
  } catch (error) {
    if (generation === mediaScanGeneration) {
      lastProjectScan = null;
      cachedProjectEntries = { projectId: null, byId: new Map() };
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

async function placeProjectItem(payload, connection) {
  const revision = projectContextRevision;
  return placement.placeProjectItem(payload, () =>
    revision === projectContextRevision && ws === connection && connection.readyState === WebSocket.OPEN);
}

async function openInSourceMonitor(payload) {
  return sourceMedia.openInSourceMonitor(payload);
}

async function resolveAudioPreview(payload) {
  return sourceMedia.resolveAudioPreview(payload);
}

async function applyEffectToSelection(payload) {
  const catalog = await ensureCatalogScanned(false);
  return effects.applyEffectToSelection(payload, catalog);
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
      const items = await ensureCatalogScanned(false);
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
      await messageHandlers.get(msg?.type)?.(msg, connection);
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

// Runs a command and acknowledges it with `{ ok, message }`. The reply is
// dropped once a newer connection replaced `connection`.
async function replyWithOutcome(connection, ackType, requestId, run) {
  let ok = false;
  let message;
  try {
    message = await run();
    ok = true;
  } catch (error) {
    message = error?.message || String(error);
  }
  if (ws === connection) {
    setLog(message);
    sendJson({ type: ackType, requestId, ok, message });
  }
}

// Ignores commands without a usable request ID, which the helper could not match to a reply.
function withRequestId(handle) {
  return (msg, connection) => (Number.isSafeInteger(msg.requestId) ? handle(msg, connection) : undefined);
}

// Helper commands by message type. Each handler receives the parsed message and its connection.
const messageHandlers = new Map([
  ["request_project_items", withRequestId(async (msg, connection) => {
    mediaRequestId = msg.requestId;
    void syncProjectItems(connection, mediaRequestId, false, true);
  })],
  ["place_project_item", withRequestId((msg, connection) =>
    replyWithOutcome(connection, "placement_ack", msg.requestId, async () => {
      if (placementInProgress) throw new Error("A media placement is already in progress.");
      placementInProgress = true;
      try {
        return await placeProjectItem(msg, connection);
      } finally {
        placementInProgress = false;
      }
    })
  )],
  ["open_in_source_monitor", withRequestId((msg, connection) =>
    replyWithOutcome(connection, "source_monitor_ack", msg.requestId, () => openInSourceMonitor(msg))
  )],
  ["request_audio_preview", withRequestId(async (msg, connection) => {
    let source = null;
    let message = "";
    try {
      source = await resolveAudioPreview(msg);
    } catch (error) {
      message = error.message || "Premiere could not resolve the audio preview.";
    }
    if (ws === connection) {
      sendJson({ type: "audio_preview_ack", requestId: msg.requestId, source, message });
    }
  })],
  ["marker_command", withRequestId(async (msg, connection) => {
    const isCurrent = () => ws === connection && connection.readyState === WebSocket.OPEN;
    let result = null;
    let message = "";
    try {
      result = await runMarkerCommand(ppro, msg, isCurrent);
    } catch (error) {
      message = error?.message || String(error);
    }
    if (isCurrent()) sendJson({ type: "marker_ack", requestId: msg.requestId, result, message });
  })],
  ["run_action", withRequestId((msg, connection) => {
    const isConnected = () => ws === connection && connection.readyState === WebSocket.OPEN;
    const watchTimeline = () => {
      const revision = projectContextRevision;
      return () => revision === projectContextRevision && isConnected();
    };
    const isCurrent = watchTimeline();
    const placing = msg.action === "add_adjustment_layer";
    return replyWithOutcome(connection, "action_ack", msg.requestId, async () => {
      if (placing && placementInProgress) throw new Error("A timeline placement is already in progress.");
      if (placing) placementInProgress = true;
      try {
        return await runAction(ppro, msg.action, isCurrent, {
          storage: globalThis.localStorage,
          findSource: async (project, matchesClip) =>
            (await collectProjectEntries(project, { matchesClip }))?.found,
          readTicks, readItemRange, readSingleTrack, trackLayout, trackLayoutsEqual, isConnected, watchTimeline
        });
      } finally {
        if (placing) placementInProgress = false;
      }
    });
  })],
  ["apply_effect", async (msg, connection) => {
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
  }],
  ["request_catalog", async () => {
    const items = await scanPremiereCatalog();
    sendJson({ type: "catalog", effects: items });
  }]
]);

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
    await ensureCatalogScanned(false);
  } catch (_) {}
  if (started) connectBridge();
}

function stopPlugin() {
  started = false;
  ++mediaScanGeneration;
  ++projectContextRevision;
  catalogScanPromise = null;
  lastProjectScan = null;
  cachedProjectEntries = { projectId: null, byId: new Map() };
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
