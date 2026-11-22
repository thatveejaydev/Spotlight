"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const { test } = require("node:test");
const vm = require("node:vm");

const pluginPath = require.resolve("../index.js");
const pluginSource = fs.readFileSync(pluginPath, "utf8");
const pluginRequire = createRequire(pluginPath);
const time = (ticks) => ({ ticks: String(ticks) });
const span = (start, end) => ({ getStartTime: () => time(start), getEndTime: () => time(end) });
class VideoClipTrackItem {}
class AudioClipTrackItem {}
const videoItem = (fields) => Object.assign(new VideoClipTrackItem(), fields);
const clipFields = (index, start, end, item) => ({
  ...span(start, end),
  getTrackIndex: () => index,
  getProjectItem: () => item,
  isAdjustmentLayer: () => item.uid === "layer"
});
const videoClip = (index, start, end, item = { getId: () => "footage" }) =>
  videoItem(clipFields(index, start, end, item));
// Premiere's audio clips have isAdjustmentLayer too.
const audioClip = (index, start, end, item = { getId: () => "footage" }) =>
  Object.assign(new AudioClipTrackItem(), clipFields(index, start, end, item));

function track({ clips = [], transitions = [], locked = false, exposeLock = true } = {}) {
  return {
    clips, locked,
    isLocked: exposeLock ? () => locked : undefined,
    getTrackItems: (type) => type === 1 ? clips : transitions
  };
}

function fixture() {
  let lockHeld = false;
  let activeProject;
  let invalidateTimeline;
  const messages = [];
  const storage = new Map();
  const attempts = [];
  const placed = [];
  const labels = [];
  const hooks = {};
  const source = { inPoint: time(30), outPoint: time(80) };
  const mark = (apply) => {
    assert.ok(lockHeld);
    return () => { apply(); return true; };
  };
  const layer = {
    id: "item-1",
    uid: "layer",
    name: "My Look",
    getId() { return this.id; },
    clip: {
      isSequence: () => false,
      getInPoint: () => source.inPoint,
      getOutPoint: () => source.outPoint,
      createSetInOutPointsAction: (from, to) => mark(() => {
        source.inPoint = from;
        source.outPoint = to;
        hooks.marksChanged?.();
      }),
      createClearInOutPointsAction: () => mark(() => {
        source.inPoint = source.outPoint = null;
      }),
      createSetInPointAction: (from) => mark(() => { source.inPoint = from; }),
      createSetOutPointAction: (to) => mark(() => { source.outPoint = to; })
    }
  };
  const root = { items: [layer] };
  const selected = videoClip(0, 100n, 200n);
  const sequence = {
    guid: "sequence",
    selected: [selected],
    video: [track({ clips: [selected] }), track()],
    getSelection() { return { getTrackItems: () => this.selected }; },
    getVideoTrackCount() {
      hooks.readTrackCount?.();
      return this.video.length;
    },
    getVideoTrack(index) { return this.video[index]; },
    getAudioTrackCount: () => 1,
    getTimebase: () => "10"
  };
  const project = {
    guid: "project",
    getActiveSequence: () => sequence,
    getRootItem: () => ({ getItems: () => root.items }),
    lockedAccess(callback) {
      assert.ok(!lockHeld);
      lockHeld = true;
      try { callback(); } finally { lockHeld = false; }
    },
    executeTransaction(callback, label) {
      assert.ok(lockHeld);
      const actions = [];
      callback({ addAction(action) { actions.push(action); return true; } });
      const committed = actions.every((action) => action());
      if (committed) {
        labels.push(label);
        invalidateTimeline();
      }
      return committed;
    }
  };
  activeProject = project;
  const edit = (method, item, from, index, audio, limitShift) => {
    assert.ok(lockHeld);
    assert.equal(item, layer);
    assert.equal(audio, 0);
    attempts.push({ method, index, limitShift });
    if (sequence.video[index]?.locked) throw new Error("Track is locked.");
    return () => {
      if (hooks.rejectPlacement) return false;
      if (method === "insert") {
        assert.equal(limitShift, true);
        assert.equal(index, sequence.video.length);
        sequence.video.push(track());
      }
      const start = BigInt(from.ticks);
      const length = BigInt(source.outPoint.ticks) - BigInt(source.inPoint.ticks);
      const placement = [index, start, start + length + (hooks.placementDrift ?? 0n)];
      sequence.video[index].clips.push({
        ...videoClip(index, start, placement[2], item),
        getEndTime: () => time(placement[2]),
        createSetEndAction: (to) => mark(() => { placement[2] = BigInt(to.ticks); })
      });
      placed.push(placement);
      hooks.afterPlacement?.(index);
      return true;
    };
  };
  const ppro = {
    Project: { getActiveProject: () => activeProject },
    FolderItem: { cast: (item) => item.children && { getItems: () => item.children } },
    ClipProjectItem: { cast: (item) => item.clip },
    UniqueSerializeable: { cast: (item) => ({ getUniqueID: () => item.uid }) },
    VideoClipTrackItem,
    AudioClipTrackItem,
    TickTime: { createWithTicks: time },
    Constants: { MediaType: { VIDEO: "video" }, TrackItemType: { CLIP: 1, TRANSITION: 2 } },
    SequenceEditor: { getEditor: () => ({
      createOverwriteItemAction: (...args) => edit("overwrite", ...args),
      createInsertProjectItemAction: (...args) => edit("insert", ...args)
    }) }
  };
  class WebSocket {
    static OPEN = 1;
    readyState = 1;
    send(value) { messages.push(JSON.parse(value)); }
  }
  const context = vm.createContext({
    require: (name) => {
      if (name === "premierepro") return ppro;
      if (name === "uxp") return { entrypoints: { setup() {} } };
      return pluginRequire(name);
    },
    document: { readyState: "loading", addEventListener() {}, getElementById: () => null },
    localStorage: { getItem: (key) => storage.get(key), setItem: (key, value) => storage.set(key, value) },
    WebSocket, console
  });
  const bridge = vm.runInContext(pluginSource + `
    connectBridge();
    ({ send: (data) => ws.onmessage({ data }), invalidate: () => ++projectContextRevision })
  `, context);
  invalidateTimeline = bridge.invalidate;
  let requestId = 0;
  async function run(action) {
    const id = ++requestId;
    await bridge.send(JSON.stringify({ type: "run_action", requestId: id, action }));
    const reply = messages.find((message) => message.requestId === id);
    assert.equal(reply.type, "action_ack");
    return reply;
  }
  return {
    root, layer, selected, sequence, project, source, attempts, placed, labels, hooks,
    setProject: (current) => { activeProject = current; },
    notifyProjectChanged: invalidateTimeline,
    add: () => run("add_adjustment_layer"),
    async register(items = [videoClip(1, 500n, 550n, layer)]) {
      sequence.selected = items;
      try { return await run("register_adjustment_layer"); }
      finally { sequence.selected = [selected]; }
    }
  };
}

test("prompts for a real layer and remembers its identity after a project reopen and bin move", async () => {
  const f = fixture();
  assert.match((await f.add()).message, /then run Register Adjustment Layer/);
  assert.equal((await f.register([f.selected])).ok, false);
  assert.equal((await f.register([f.selected, f.selected])).ok, false);
  assert.equal((await f.register()).ok, true);
  f.layer.id = "reopened-id";
  f.layer.name = "Renamed layer";
  f.root.items = [{ getId: () => "bin", children: [f.layer] }];
  f.setProject({ ...f.project });
  assert.equal((await f.add()).ok, true);
  assert.deepEqual(f.placed, [[1, 100n, 200n]]);
  f.setProject({ ...f.project, guid: "another-project" });
  assert.equal((await f.add()).ok, false);
  f.setProject(f.project);
  f.root.items = [];
  assert.match((await f.add()).message, /then run Register Adjustment Layer/);
});

test("spans mixed-track video selections and gaps at exact ticks and skips locked and occupied tracks", async () => {
  const f = fixture();
  await f.register();
  const start = 9007199254741000n;
  const end = start + 200n;
  const low = videoClip(0, start, start + 30n);
  const high = videoClip(1, end - 20n, end);
  f.sequence.selected = [high, span(0, end + 300n), low];
  const edges = [videoClip(4, start - 10n, start), videoClip(4, end, end + 10n)];
  f.sequence.video = [
    track({ clips: [low] }),
    track({ clips: [high] }),
    track({ locked: true }),
    track({ transitions: [span(start + 50n, start + 60n)] }),
    track({ clips: [...edges] })
  ];
  assert.match((await f.add()).message, /on V5/);
  assert.deepEqual(f.attempts, [{ method: "overwrite", index: 4, limitShift: undefined }]);
  assert.deepEqual(f.placed, [[4, start, end]]);
  assert.deepEqual(f.sequence.video[4].clips.slice(0, 2), edges);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
});

test("tries a new top track when a locked track cannot be read and other tracks overlap", async () => {
  const f = fixture();
  await f.register();
  f.source.inPoint = null;
  f.sequence.video.push(track({ clips: [videoClip(2, 190n, 220n)] }));
  f.sequence.video[1] = track({ locked: true, exposeLock: false });
  assert.match((await f.add()).message, /on V4/);
  assert.deepEqual(f.attempts, [
    { method: "overwrite", index: 1, limitShift: undefined },
    { method: "insert", index: 3, limitShift: true }
  ]);
  assert.deepEqual(f.placed, [[3, 100n, 200n]]);
  assert.deepEqual(f.source, { inPoint: null, outPoint: time(80) });
});

test("linked and longer audio clips don't move or stretch the layer", async () => {
  for (const audio of [audioClip(1, 100n, 200n), audioClip(5, 50n, 400n)]) {
    const f = fixture();
    await f.register();
    f.sequence.video.push(track());
    f.sequence.selected = [audio, f.selected];
    assert.match((await f.add()).message, /on V2/);
    assert.deepEqual(f.placed, [[1, 100n, 200n]]);
  }
});

test("rejects empty, audio-only, and stale selections before editing", async () => {
  const f = fixture();
  await f.register();
  for (const items of [[], [span(0, 100)], [audioClip(0, 100n, 200n)]]) {
    f.sequence.selected = items;
    assert.match((await f.add()).message, /Select at least one video clip/);
  }
  f.sequence.selected = [f.selected];
  f.hooks.marksChanged = () => { f.sequence.selected = [videoClip(0, 120n, 230n)]; };
  assert.match((await f.add()).message, /timeline changed/);
  assert.deepEqual(f.attempts, []);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
});

test("places the layer despite delayed project notifications before and after source sizing", async () => {
  const f = fixture();
  await f.register();
  let pendingNotification = true;
  let notifications = 0;
  f.hooks.marksChanged = () => { pendingNotification = true; };
  f.hooks.readTrackCount = () => {
    if (!pendingNotification) return;
    pendingNotification = false;
    notifications++;
    f.notifyProjectChanged();
  };

  assert.match((await f.add()).message, /on V2/);
  assert.equal(notifications, 2);
  assert.deepEqual(f.placed, [[1, 100n, 200n]]);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
});

test("aborts if the destination becomes occupied while sizing the source", async () => {
  const f = fixture();
  await f.register();
  f.hooks.marksChanged = () => {
    f.sequence.video[1].clips.push(videoClip(1, 140n, 160n));
    f.notifyProjectChanged();
  };

  assert.match((await f.add()).message, /timeline changed/);
  assert.deepEqual(f.attempts, []);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
});

test("restores source marks on edit refusal and does not retry an ambiguous placement", async () => {
  const f = fixture();
  await f.register();
  f.hooks.rejectPlacement = true;
  assert.equal((await f.add()).ok, false);
  assert.deepEqual(f.placed, []);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
  f.attempts.length = 0;
  f.hooks.rejectPlacement = false;
  f.hooks.placementDrift = 10n;
  const reply = await f.add();
  assert.equal(reply.ok, false);
  assert.match(reply.message, /expected bounds/);
  assert.equal(f.attempts.length, 1);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
});

test("keeps source marks untouched when they already match the requested duration", async () => {
  const f = fixture();
  await f.register();
  f.source.outPoint = time(130);
  assert.equal((await f.add()).ok, true);
  assert.deepEqual(f.labels, ["Add Adjustment Layer"]);
  assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(130) });
});

test("extends layers placed one to three frames short without moving the following clip", async () => {
  for (const frames of [1n, 2n, 3n]) {
    const f = fixture();
    await f.register();
    const neighbor = videoClip(1, 200n, 250n);
    f.sequence.video[1].clips.push(neighbor);
    f.hooks.placementDrift = -10n * frames;

    assert.match((await f.add()).message, /on V2/);
    assert.deepEqual(f.placed, [[1, 100n, 200n]]);
    assert.equal(f.sequence.video[1].clips[0], neighbor);
    assert.equal(f.attempts.length, 1);
    assert.deepEqual(f.source, { inPoint: time(30), outPoint: time(80) });
  }
});

test("keeps a short layer in place if the extension would overlap a new transition", async () => {
  const f = fixture();
  await f.register();
  f.hooks.placementDrift = -30n;
  f.hooks.afterPlacement = (index) => {
    f.sequence.video[index].getTrackItems = (type) =>
      type === 1 ? f.sequence.video[index].clips : [span(180n, 190n)];
  };

  const reply = await f.add();
  assert.equal(reply.ok, false);
  assert.match(reply.message, /Premiere added a short adjustment layer/);
  assert.deepEqual(f.placed, [[1, 100n, 170n]]);
  assert.equal(f.attempts.length, 1);
  assert.ok(!f.labels.includes("Extend Adjustment Layer"));
});

test("places an adjustment layer over text and generated clips without a project item", async () => {
  for (const getProjectItem of [
    () => null,
    () => ({ getId: () => "" }),
    () => { throw new Error("No project item"); }
  ]) {
    const f = fixture();
    await f.register();
    const textLayer = videoItem({
      ...span(100n, 220n),
      getTrackIndex: () => 0,
      getProjectItem,
      isAdjustmentLayer: () => false
    });
    f.sequence.selected = [textLayer];
    f.sequence.video = [track({ clips: [textLayer] }), track()];

    const reply = await f.add();
    assert.equal(reply.ok, true);
    assert.match(reply.message, /on V2/);
    assert.deepEqual(f.placed, [[1, 100n, 220n]]);
  }
});

test("detects selection replacement and still rejects invalid clip track bounds", async () => {
  const f = fixture();
  await f.register();
  const textLayer = videoItem({
    ...span(100n, 200n),
    getTrackIndex: () => 0,
    getProjectItem: () => null,
    isAdjustmentLayer: () => false
  });
  f.sequence.selected = [textLayer];
  f.hooks.marksChanged = () => { f.sequence.selected = [videoClip(0, 100n, 200n)]; };
  assert.match((await f.add()).message, /timeline changed/);
  assert.deepEqual(f.attempts, []);

  f.hooks.marksChanged = null;
  f.sequence.selected = [videoItem({
    ...span(100n, 200n),
    getTrackIndex: () => -1,
    getProjectItem: () => null,
    isAdjustmentLayer: () => false
  })];
  assert.match((await f.add()).message, /Premiere could not verify the selected video clips/);
});
