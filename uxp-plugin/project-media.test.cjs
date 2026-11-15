const assert = require("node:assert/strict");
const fs = require("node:fs");
const { test } = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(require.resolve("./index.js"), "utf8");
const time = (ticks) => ({ ticks: String(ticks) });
const empty = () => track();

function track({ clips = [], transitions = [], locked = false } = {}) {
  return {
    isLocked: () => locked,
    getTrackItems: (type) => (type === 1 ? clips : transitions).map(([start, end]) => ({
      getStartTime: () => time(start),
      getEndTime: () => time(end)
    }))
  };
}

function fixture() {
  const commits = [];
  const messages = [];
  const socket = { readyState: 1, send: (text) => messages.push(JSON.parse(text)) };
  const seq = {
    guid: "timeline",
    getProjectItem: () => ({ getId: () => "self" }),
    getPlayerPosition: () => time(100),
    getTimebase: () => "10",
    video: [track({ clips: [[0, 100], [150, 200]] }), empty()],
    audio: [empty()],
    getVideoTrackCount() { return this.video.length; },
    getVideoTrack(i) { return this.video[i]; },
    getAudioTrackCount() { return this.audio.length; },
    getAudioTrack(i) { return this.audio[i]; }
  };
  const clip = {
    isSequence: () => false,
    isOffline: () => false,
    getComponentChain: (type) => type === "video" ? {} : null,
    getInPoint: () => time(50),
    getOutPoint: () => time(100)
  };
  const item = { name: "Take.mov", getId: () => "take", clip };
  const nest = {
    name: "Draft",
    getId: () => "draft",
    clip: { isSequence: () => true, getSequence: () => ({ guid: "draft", getEndTime: () => time(50) }) }
  };
  const self = { ...nest, getId: () => "self", clip: { ...nest.clip, getSequence: () => seq } };
  const subBin = { name: "Secondary", getId: () => "sub", children: [
    { ...item, getId: () => "alternate" }
  ] };
  const bin = { name: "Footage", getId: () => "bin", children: [subBin, item] };
  const unsupported = { name: "Other item", getId: () => "other" };
  const root = { children: [bin, nest, self, unsupported] };
  let lockHeld = false;
  const project = {
    guid: "project",
    getRootItem: () => ({ getItems: () => root.children }),
    getActiveSequence: () => seq,
    lockedAccess(callback) {
      lockHeld = true;
      try { callback(); } finally { lockHeld = false; }
    },
    executeTransaction(callback, label) {
      assert.ok(lockHeld);
      callback({ addAction(action) { commits.push({ action, label }); return true; } });
      return true;
    }
  };
  const ppro = {
    Project: { getActiveProject: () => project },
    FolderItem: { cast(item) {
      if (!item.children) throw new Error("not a bin");
      return { getItems: () => item.children };
    } },
    ClipProjectItem: { cast: (item) => item.clip },
    Constants: { TrackItemType: { CLIP: 1, TRANSITION: 2 }, MediaType: { VIDEO: "video", AUDIO: "audio" } },
    SequenceEditor: { getEditor: () => ({
      createOverwriteItemAction(item, position, video, audio) {
        assert.ok(lockHeld);
        return { itemId: item.getId(), position: position.ticks, video, audio };
      }
    }) }
  };
  const context = vm.createContext({
    require: (id) => id === "premierepro" ? ppro : { entrypoints: { setup() {} } },
    document: { readyState: "loading", addEventListener() {}, getElementById: () => null },
    WebSocket: { OPEN: 1 }, console, socket
  });
  const exports = vm.runInContext(
    source + "\nws = socket; ({ placeProjectItem, syncProjectItems, applyEffectToSelection, openInSourceMonitor, ensureCatalogScanned, scanPremiereCatalog });", context
  );
  const { placeProjectItem, syncProjectItems, applyEffectToSelection, openInSourceMonitor } = exports;
  return {
    ...exports,
    clip, seq, bin, root, project, commits, messages, ppro, applyEffectToSelection,
    place(itemId = "take") {
      return placeProjectItem({ itemId, projectId: "project", sequenceId: "timeline" }, socket);
    },
    openSource(itemId = "take") {
      return openInSourceMonitor({ itemId, projectId: "project" });
    },
    scan(id) { return syncProjectItems(socket, id); }
  };
}

test("indexes nested bins, sequences, and unsupported Project panel items", async () => {
  const f = fixture();
  await f.scan(7);
  const result = f.messages[0];
  assert.equal(result.requestId, 7);
  assert.equal(result.sequenceId, "timeline");
  assert.deepEqual(result.items.filter((item) => item.name === "Take.mov")
    .map(({ itemId, binPath }) => [itemId, binPath]).sort(), [
    ["alternate", "Footage / Secondary"], ["take", "Footage"]
  ]);
  assert.deepEqual(result.items.filter((item) => item.kind === "sequence")
    .map((item) => item.itemId), ["draft"]);
  assert.deepEqual(result.items.filter((item) => item.kind === "bin")
    .map(({ itemId, binPath }) => [itemId, binPath]), [["bin", "Project root"], ["sub", "Footage"]]);
  assert.equal(result.items.find((item) => item.itemId === "self").kind, "activeSequence");
  assert.equal(result.items.find((item) => item.itemId === "other").kind, "other");
});

test("indexes offline media and rejects non-placeable items before committing", async () => {
  const f = fixture();
  f.clip.isOffline = () => true;
  await f.scan(7);
  assert.equal(f.messages[0].items.find((item) => item.itemId === "take").offline, true);
  await assert.rejects(f.place("bin"), /Bins can't be placed on the timeline\. Bins are search only\./);
  await assert.rejects(f.place("other"), /project item can't be placed/);
  await assert.rejects(f.place("self"), /nest into itself/);
  await assert.rejects(f.place(), /offline/);
  assert.deepEqual(f.commits, []);
});

test("an older scan cannot replace a newer project response", async () => {
  const f = fixture();
  const stale = f.scan(1);
  f.ppro.Project.getActiveProject = () => null;
  await Promise.all([stale, f.scan(2)]);
  assert.deepEqual(f.messages.map(({ requestId, projectId }) => [requestId, projectId]), [[2, null]]);
});

test("places at the playhead in an exact-fit gap using one transaction", async () => {
  const f = fixture();
  assert.match(await f.place(), /Placed/);
  assert.deepEqual(f.commits, [{
    action: { itemId: "take", position: "100", video: 0, audio: 0 },
    label: "Place Take.mov"
  }]);
});

test("skips short gaps, transitions and locked tracks", async () => {
  const f = fixture();
  f.seq.video = [
    track({ clips: [[149, 160]] }),
    track({ transitions: [[110, 120]] }),
    track({ locked: true }),
    empty()
  ];
  await f.place();
  assert.equal(f.commits[0].action.video, 3);
});

test("requests fresh indices when a nest has no safe destination", async () => {
  const f = fixture();
  f.seq.video = [track({ clips: [[90, 160]] })];
  f.seq.audio = [empty(), track({ clips: [[90, 160]] })];
  await f.place("draft");
  assert.deepEqual(f.commits[0].action, {
    itemId: "draft", position: "100", video: 1, audio: 2
  });
});

test("audio-only sources avoid occupied audio lanes", async () => {
  const f = fixture();
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.seq.audio = [track({ clips: [[90, 160]] }), empty()];
  await f.place();
  assert.equal(f.commits[0].action.audio, 1);
});

test("audio-only media uses the first gap that fits even when later tracks overlap", async () => {
  const f = fixture();
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.clip.getInPoint = () => null;
  f.clip.getOutPoint = () => null;
  f.clip.getMedia = () => ({ duration: Promise.resolve(time(50)) });
  f.seq.audio = [
    track({ clips: [[149, 180]] }),
    track({ clips: [[20, 100], [150, 180]] }),
    track({ clips: [[110, 130]] }),
    empty()
  ];
  await f.place();
  assert.equal(f.commits[0].action.audio, 1);
});

test("WAV with unavailable source bounds uses A1 when all its clips end before the playhead", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "C:\\Media\\sting.wav";
  // A missing video component chain can reject instead of returning null.
  f.clip.getComponentChain = (type) => {
    if (type === "video") throw new Error("No video component chain");
    return {};
  };
  f.clip.getInPoint = () => null;
  f.clip.getOutPoint = () => null;
  f.seq.audio = [
    track({ clips: [[40, 85]] }),
    track({ clips: [[60, 200]] }),
    track({ clips: [[0, 40]] }),
    empty()
  ];
  await f.place();
  assert.deepEqual(f.commits[0].action, {
    itemId: "take", position: "100", video: 0, audio: 0
  });
});

test("audio with unavailable duration skips a track with a future clip", async () => {
  const f = fixture();
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.clip.getOutPoint = () => null;
  f.seq.audio = [track({ clips: [[250, 270]] }), track({ clips: [[0, 90]] }), empty()];
  await f.place();
  assert.equal(f.commits[0].action.audio, 1);
});

test("audio can use A1 when its prior clip bounds come from start plus duration", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "C:\\Media\\bamboo hit_sfx.mp3";
  f.seq.audio = [{
    async getTrackItems(type) {
      return type === 1 ? [{ getStartTime: () => time(40), getDuration: () => time(45) }] : [];
    }
  }, track({ clips: [[60, 200]] }), empty(), empty()];
  await f.place();
  assert.equal(f.commits[0].action.audio, 0);
});

test("audio can use A1 when Premiere omits its empty transition list", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "C:\\Media\\achievement_sfx.mp3";
  const first = track({ clips: [[40, 85]] });
  f.seq.audio = [
    { getTrackItems: (type) => type === 1 ? [...first.getTrackItems(type), null] : undefined },
    track({ clips: [[60, 200]] }),
    empty(),
    empty()
  ];
  await f.place();
  assert.equal(f.commits[0].action.audio, 0);
});

test("placement status distinguishes Premiere routing to A4 from requesting A1", async () => {
  const f = fixture();
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.seq.audio = [track({ clips: [[40, 85]] }), track({ clips: [[60, 200]] }), empty(), empty()];
  const execute = f.project.executeTransaction;
  f.project.executeTransaction = (callback, label) => {
    const committed = execute(callback, label);
    f.seq.audio[3] = {
      getTrackItems: (type) => type === 1 ? [{
        getStartTime: () => time(100), getEndTime: () => time(150),
        getProjectItem: () => ({ getId: () => "take" })
      }] : []
    };
    return committed;
  };
  assert.match(await f.place(), /on A4 \(requested A1\)/);
  assert.equal(f.commits[0].action.audio, 0);
});

test("recognizes the MP3 filename when Premiere omits the media path and audio chain", async () => {
  const f = fixture();
  f.bin.children[1].name = "achievement_sfx.mp3";
  f.clip.getMediaFilePath = () => "";
  f.clip.getComponentChain = (type) => type === "audio" ? null : {};
  f.seq.audio = [empty(), track({ clips: [[60, 200]] }), empty(), empty()];

  assert.match(await f.place(), /achievement_sfx\.mp3.*requested A1/);
  assert.equal(f.commits[0].action.audio, 0);
});

test("linked media checks both track groups", async () => {
  const f = fixture();
  f.clip.getComponentChain = () => ({});
  f.seq.audio = [track({ transitions: [[100, 105]] }), empty()];
  await f.place();
  assert.equal(f.commits[0].action.video, 0);
  assert.equal(f.commits[0].action.audio, 1);
});

test("linked media uses the gap on A1 even when A2 overlaps", async () => {
  const f = fixture();
  f.bin.children[1].name = "reference.mp4";
  f.clip.getComponentChain = () => ({});
  f.seq.video = [track({ clips: [[0, 90]] }), track({ clips: [[95, 200]] })];
  f.seq.audio = [
    track({ clips: [[0, 90]] }),
    track({ clips: [[95, 200]] }),
    empty()
  ];

  await f.place();
  assert.deepEqual(f.commits[0].action, {
    itemId: "take", position: "100", video: 0, audio: 0
  });
});

test("stills with unknown duration use an empty track", async () => {
  const f = fixture();
  f.clip.getOutPoint = () => null;
  await f.place();
  assert.equal(f.commits[0].action.video, 1);
});

test("video effects still apply to the timeline selection", async () => {
  const f = fixture();
  const trackItem = { getComponentChain: () => ({
    createAppendComponentAction: (component) => ({ component })
  }) };
  f.seq.getSelection = () => ({ getTrackItems: () => [trackItem] });
  f.ppro.VideoFilterFactory = { createComponent: (name) => name };
  assert.match(await f.applyEffectToSelection({
    name: "Blur", matchName: "test.blur", effectType: "videoEffect"
  }), /Applied "Blur"/);
  assert.deepEqual(JSON.parse(JSON.stringify(f.commits)), [{
    action: { component: "test.blur" }, label: "Apply Blur"
  }]);
});

test("rejects stale context, unavailable items and timeline changes before committing", async () => {
  const f = fixture();
  await assert.rejects(f.place("self"), /nest into itself/);
  await assert.rejects(f.place("missing"), /removed/);
  f.clip.isOffline = () => true;
  await assert.rejects(f.place(), /offline/);
  f.clip.isOffline = () => false;
  let reads = 0;
  f.seq.getVideoTrack = () => reads++ === 0 ? empty() : track({ clips: [[95, 155]] });
  await assert.rejects(f.place(), /Timeline changed/);
  f.project.getActiveSequence = () => ({ ...f.seq, guid: "other" });
  await assert.rejects(f.place(), /Active sequence changed/);
  f.project.getActiveSequence = () => null;
  await assert.rejects(f.place(), /Open a timeline/);
  f.ppro.Project.getActiveProject = () => ({ ...f.project, guid: "other project" });
  await assert.rejects(f.place(), /Active project changed/);
  assert.equal(f.commits.length, 0);
});

test("reports host rejection when placement needs fresh tracks", async () => {
  const f = fixture();
  f.seq.video = [track({ clips: [[90, 160]] })];
  f.project.executeTransaction = () => false;
  await assert.rejects(f.place(), /could not place media on new tracks/);
  assert.equal(f.commits.length, 0);
});

test("opens media and sequences in the Source Monitor without touching the timeline", async () => {
  const f = fixture();
  const opened = [];
  f.ppro.SourceMonitor = { openProjectItem: async (item) => { opened.push(item.getId()); return true; } };
  f.project.getActiveSequence = () => null;
  assert.match(await f.openSource(), /Opened "Take\.mov" in the Source Monitor/);
  await f.openSource("draft");
  await f.openSource("self");
  f.clip.isOffline = () => true;
  await f.openSource();
  assert.deepEqual(opened, ["take", "draft", "self", "take"]);
  assert.deepEqual(f.commits, []);
});

test("Source Monitor rejects bins, unsupported items, stale context and host failures", async () => {
  const f = fixture();
  await assert.rejects(f.openSource(), /requires Premiere Pro 25\.6/);
  const opened = [];
  f.ppro.SourceMonitor = { openProjectItem: async (item) => { opened.push(item.getId()); return true; } };
  await assert.rejects(f.openSource("bin"), /Bins can't open/);
  await assert.rejects(f.openSource("other"), /can't open in the Source Monitor/);
  await assert.rejects(f.openSource("missing"), /removed/);
  f.ppro.SourceMonitor.openProjectItem = async () => false;
  await assert.rejects(f.openSource(), /could not open "Take\.mov"/);
  f.ppro.Project.getActiveProject = () => ({ ...f.project, guid: "other project" });
  await assert.rejects(f.openSource(), /Active project changed/);
  assert.deepEqual(opened, []);
});

test("ensureCatalogScanned reuses cached effects unless forced to rescan", async () => {
  const f = fixture();
  let videoScans = 0;
  f.ppro.VideoFilterFactory = {
    getMatchNames: () => { videoScans++; return ["test.blur"]; },
    getDisplayNames: () => ["Blur"]
  };
  const first = await f.ensureCatalogScanned(false);
  const second = await f.ensureCatalogScanned(false);
  assert.equal(first.length, 1);
  assert.equal(second.length, 1);
  assert.equal(videoScans, 1);
  await f.scanPremiereCatalog();
  assert.equal(videoScans, 2);
});

test("place and Source Monitor use cached project item index after sync without re-walking bins", async () => {
  const f = fixture();
  let rootWalks = 0;
  const baseRoot = f.project.getRootItem;
  f.project.getRootItem = () => {
    rootWalks++;
    return baseRoot();
  };
  f.ppro.SourceMonitor = { openProjectItem: async () => true };

  await f.scan(10);
  assert.equal(rootWalks, 1);

  await f.place("alternate");
  await f.openSource("alternate");
  assert.equal(rootWalks, 1);

  f.root.children.push({ name: "Fresh.mov", getId: () => "fresh", clip: f.clip });
  await f.place("fresh");
  assert.equal(rootWalks, 2);
});

test("skips unused track groups, locked tracks, and unaffected trailing tracks during placement verification", async () => {
  const f = fixture();
  let videoReads = 0;
  let audioReads = 0;
  let lockedTrackItemReads = 0;
  const origGetVideo = f.seq.getVideoTrack.bind(f.seq);
  const origGetAudio = f.seq.getAudioTrack.bind(f.seq);
  f.seq.video = [
    empty(),
    { isLocked: () => true, getTrackItems: () => { lockedTrackItemReads++; return []; } },
    track({ clips: [[95, 155]] })
  ];
  f.seq.audio = [
    empty(),
    empty(),
    empty()
  ];
  f.seq.getVideoTrack = (i) => { videoReads++; return origGetVideo(i); };
  f.seq.getAudioTrack = (i) => { audioReads++; return origGetAudio(i); };

  await f.place();
  // First pass reads V1..V3 (3), chosen video=0 so verification re-reads V1 only (+1 = 4); audio skipped (0).
  assert.equal(videoReads, 4);
  assert.equal(audioReads, 0);

  // Audio-only clip skips all video tracks and locked tracks never query getTrackItems.
  videoReads = 0;
  audioReads = 0;
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.seq.audio = [
    { isLocked: () => true, getTrackItems: () => { lockedTrackItemReads++; return []; } },
    empty(),
    empty()
  ];
  await f.place();
  assert.equal(videoReads, 0);
  // 3 initial track scans + 2 verification scans (A1..A2) + 2 post-commit placedAudioTracks checks (skipping locked A1).
  assert.equal(audioReads, 7);
  assert.equal(lockedTrackItemReads, 0);
});
