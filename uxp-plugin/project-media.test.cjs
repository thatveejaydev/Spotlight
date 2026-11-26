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
    require: (id) => {
      if (id === "premierepro") return ppro;
      if (id === "uxp") return { entrypoints: { setup() {} } };
      return require(id);
    },
    document: { readyState: "loading", addEventListener() {}, getElementById: () => null },
    WebSocket: { OPEN: 1 }, console, socket, setTimeout
  });
  const exports = vm.runInContext(
    source + "\nws = socket; ({ placeProjectItem, syncProjectItems, applyEffectToSelection, openInSourceMonitor, openSequence: (payload) => sourceMedia.openSequenceInTimeline(payload), resolveAudioPreview, ensureCatalogScanned, scanPremiereCatalog, markProjectChanged: () => ++projectContextRevision, importAndPlace, importAndOpen });", context
  );
  const { placeProjectItem, syncProjectItems, applyEffectToSelection, openInSourceMonitor } = exports;
  return {
    ...exports,
    clip, seq, bin, root, project, commits, messages, ppro, applyEffectToSelection, connection: socket,
    place(itemId = "take") {
      return placeProjectItem({ itemId, projectId: "project", sequenceId: "timeline" }, socket);
    },
    openSource(itemId = "take") {
      return openInSourceMonitor({ itemId, projectId: "project" });
    },
    previewAudio(itemId = "take") {
      return exports.resolveAudioPreview({ itemId, projectId: "project" });
    },
    scan(id) { return syncProjectItems(socket, id); }
  };
}

test("resolves an audio file and its In and Out points without an active timeline", async () => {
  const f = fixture();
  f.project.getActiveSequence = () => null;
  f.clip.getMediaFilePath = () => "C:\\Media\\Audio\\Original.wav";
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.clip.getInPoint = (type) => {
    assert.equal(type, "audio");
    return time(254016000000n * 2n);
  };
  f.clip.getOutPoint = (type) => {
    assert.equal(type, "audio");
    return time(254016000000n * 5n);
  };
  await f.scan(1);
  assert.equal(f.messages[0].items.find((item) => item.itemId === "take").path, undefined);
  f.project.getRootItem = () => assert.fail("the indexed item should be reused");
  const result = await f.previewAudio();
  assert.equal(result.path, "C:\\Media\\Audio\\Original.wav");
  assert.equal(result.inSeconds, 2);
  assert.equal(result.outSeconds, 5);
  assert.deepEqual(f.commits, []);
});

test("previews a standalone audio file when Premiere omits component metadata and marks", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "C:\\Media\\Music.M4A";
  f.clip.getComponentChain = () => { throw new Error("No chain"); };
  f.clip.getInPoint = () => time(-1);
  f.clip.getOutPoint = () => { throw new Error("No Out point"); };
  assert.deepEqual({ ...await f.previewAudio() }, {
    path: "C:\\Media\\Music.M4A", inSeconds: null, outSeconds: null
  });
});

test("skips video and rejects offline, missing, and stale audio sources", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "C:\\Media\\Take.mov";
  f.clip.getComponentChain = () => ({});
  assert.equal(await f.previewAudio(), null);
  assert.equal(await f.previewAudio("bin"), null);
  assert.equal(await f.previewAudio("draft"), null);

  f.clip.isOffline = () => true;
  await assert.rejects(f.previewAudio(), /offline/);
  f.clip.isOffline = () => false;
  f.clip.getComponentChain = (type) => type === "audio" ? {} : null;
  f.clip.getMediaFilePath = () => "";
  await assert.rejects(f.previewAudio(), /could not provide the audio source/);
  f.clip.getMediaFilePath = () => {
    f.ppro.Project.getActiveProject = () => null;
    return "C:\\Media\\Original.wav";
  };
  await assert.rejects(f.previewAudio(), /Active project changed/);
});

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

test("helper requests reuse the last list until the project changes", async () => {
  const f = fixture();
  let rootWalks = 0;
  const baseRoot = f.project.getRootItem;
  f.project.getRootItem = () => { rootWalks++; return baseRoot(); };
  const send = (id) => f.syncProjectItems(f.connection, id, false, true);
  await send(1);
  await send(2);
  assert.equal(rootWalks, 1);
  assert.deepEqual(f.messages.map((m) => [m.requestId, m.scanMode]), [[1, "rescanned"], [2, "reused"]]);
  assert.deepEqual(f.messages[1].items, f.messages[0].items);

  f.markProjectChanged();
  await send(3);
  assert.equal(rootWalks, 2);
  assert.equal(f.messages[2].scanMode, "rescanned");

  f.project.getActiveSequence = () => ({ ...f.seq, guid: "other" });
  await send(4);
  assert.equal(rootWalks, 3);
  assert.equal(f.messages[3].scanMode, "rescanned");

  await f.syncProjectItems(f.connection, 5, true);
  assert.equal(rootWalks, 4);
  assert.deepEqual(f.messages.slice(4).map((m) => [m.items.length, m.scanMode]),
    [[0, "scanning"], [f.messages[3].items.length, "rescanned"]]);
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

for (const file of ["sting.wav", "voice.m4a", "loop.ALAC"])
test(`${file} with unavailable source bounds uses A1 when all its clips end before the playhead`, async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => `C:\\Media\\${file}`;
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

test("opens a sequence in its own Timeline tab and brings an open one forward", async () => {
  const f = fixture();
  const calls = [];
  let active = f.seq;
  f.project.getActiveSequence = () => active;
  f.project.openSequence = async (sequence) => { calls.push(["open", sequence.guid]); return true; };
  f.project.setActiveSequence = async (sequence) => {
    calls.push(["activate", sequence.guid]);
    active = sequence;
    return true;
  };
  const open = (itemId) => f.openSequence({ itemId, projectId: "project" });

  // openSequence leaves another sequence active, so Spotlight activates the opened one.
  assert.match(await open("draft"), /Opened "Draft" in the Timeline/);
  assert.deepEqual(calls, [["open", "draft"], ["activate", "draft"]]);

  // The active sequence is already in front.
  calls.length = 0;
  active = f.seq;
  assert.match(await open("self"), /Opened "Draft" in the Timeline/);
  assert.deepEqual(calls, []);

  f.project.openSequence = async (sequence) => { active = sequence; calls.push(["open", sequence.guid]); return true; };
  await open("draft");
  assert.deepEqual(calls, [["open", "draft"]]);
  assert.deepEqual(f.commits, []);
});

test("opening in the Timeline rejects media, bins, stale context and host failures", async () => {
  const f = fixture();
  f.project.openSequence = async () => false;
  f.project.setActiveSequence = async () => false;
  const open = (itemId) => f.openSequence({ itemId, projectId: "project" });
  for (const itemId of ["take", "bin", "other"]) {
    await assert.rejects(open(itemId), /Only sequences open in the Timeline/);
  }
  await assert.rejects(open("missing"), /removed/);
  await assert.rejects(open("draft"), /could not open "Draft" in the Timeline/);
  f.project.openSequence = async () => true;
  await assert.rejects(open("draft"), /could not bring "Draft" forward/);
  f.ppro.Project.getActiveProject = () => ({ ...f.project, guid: "other project" });
  await assert.rejects(open("draft"), /Active project changed/);
});

test("reports whether each clip is video, an image or audio", async () => {
  const f = fixture();
  await f.scan(7);
  const typeOf = (messages, itemId) => messages.at(-1).items.find((item) => item.itemId === itemId).mediaType;
  // Without a media path the item name decides; sequences, bins and other items get none.
  assert.equal(typeOf(f.messages, "take"), "video");
  for (const itemId of ["draft", "bin", "other", "self"]) assert.equal(typeOf(f.messages, itemId), null);

  // The media file wins over a renamed item.
  f.clip.getMediaFilePath = async () => "D:\Graphics\Logo_Lockup.PNG";
  f.markProjectChanged();
  await f.scan(8);
  assert.equal(typeOf(f.messages, "take"), "image");
  f.clip.getMediaFilePath = async () => "D:\Music\Theme.wav";
  f.markProjectChanged();
  await f.scan(9);
  assert.equal(typeOf(f.messages, "take"), "audio");

  // A failing or empty path falls back to the name.
  f.clip.getMediaFilePath = async () => { throw new Error("unavailable"); };
  f.markProjectChanged();
  await f.scan(10);
  assert.equal(typeOf(f.messages, "take"), "video");
});

// A project whose root can make bins, and a Library file that importFiles adds to the
// Spotlight Library bin.
function libraryFixture() {
  const f = fixture();
  const libraryBin = { name: "Spotlight Library", getId: () => "library-bin", children: [] };
  const rootFolder = {
    getItems: () => f.root.children,
    createBinAction: (name, makeUnique) => ({ createBin: name, makeUnique })
  };
  f.project.getRootItem = () => rootFolder;
  const execute = f.project.executeTransaction;
  f.project.executeTransaction = function (callback, label) {
    const committed = execute.call(this, callback, label);
    if (f.commits.at(-1)?.action?.createBin) f.root.children.push(libraryBin);
    return committed;
  };
  const imports = [];
  const audioClip = (path) => ({
    isSequence: () => false,
    isOffline: () => false,
    getMediaFilePath: () => path,
    getComponentChain: (type) => (type === "audio" ? {} : null),
    getInPoint: () => time(0),
    getOutPoint: () => time(50)
  });
  f.project.importFiles = async (paths, suppressUI, bin, asNumberedStills) => {
    imports.push({ paths, suppressUI, bin: bin?.getId(), asNumberedStills });
    for (const path of paths) {
      const name = path.split("\\").pop();
      if (name !== "Ghost.wav") bin.children.push({ name, getId: () => name.toLowerCase(), clip: audioClip(path) });
    }
    return true;
  };
  const payload = (path) => ({ projectId: "project", sequenceId: "timeline", path });
  return { ...f, libraryBin, imports, payload };
}

test("the project scan reports each clip's media file", async () => {
  const f = fixture();
  f.clip.getMediaFilePath = () => "D:\\Media\\Take.mov";
  await f.scan(3);
  const items = f.messages[0].items;
  assert.equal(items.find((item) => item.itemId === "take").mediaPath, "D:\\Media\\Take.mov");
  for (const itemId of ["draft", "bin", "other"]) {
    assert.equal(items.find((item) => item.itemId === itemId).mediaPath, null);
  }
});

test("imports a Library file into the Spotlight Library bin and places it at the playhead", async () => {
  const f = libraryFixture();
  const message = await f.importAndPlace(f.payload("D:\\SFX\\Boom.wav"), f.connection);
  assert.match(message, /^Imported and placed "Boom.wav" at the playhead/);
  assert.deepEqual(f.imports, [{ paths: ["D:\\SFX\\Boom.wav"], suppressUI: true, bin: "library-bin", asNumberedStills: false }]);
  assert.deepEqual(f.commits.map((commit) => commit.label), ["Create Spotlight Library bin", "Place Boom.wav"]);
  assert.equal(f.commits[1].action.itemId, "boom.wav");

  // The same file again, spelled differently, reuses its clip and the bin.
  const again = await f.importAndPlace(f.payload("d:/sfx/BOOM.wav"), f.connection);
  assert.match(again, /^Placed "Boom.wav"/);
  assert.equal(f.imports.length, 1);
  assert.deepEqual(f.commits.map((commit) => commit.label).slice(2), ["Place Boom.wav"]);
});

test("a Library file already in the project is placed without importing it", async () => {
  const f = libraryFixture();
  f.clip.getMediaFilePath = () => "D:\\Footage\\Take.mov";
  f.project.importFiles = () => assert.fail("the existing clip should be reused");
  assert.match(await f.importAndPlace(f.payload("d:/footage/TAKE.MOV"), f.connection), /^Placed "Take.mov"/);
  assert.deepEqual(f.commits.map((commit) => commit.label), ["Place Take.mov"]);
});

test("a Library import that adds nothing, or for another project, fails before editing", async () => {
  const f = libraryFixture();
  await assert.rejects(f.importAndPlace(f.payload("D:\\SFX\\Ghost.wav"), f.connection),
    /did not add "Ghost.wav" to the project/);
  assert.deepEqual(f.commits.map((commit) => commit.label), ["Create Spotlight Library bin"]);

  await assert.rejects(f.importAndPlace({ ...f.payload("D:\\SFX\\Boom.wav"), projectId: "other" }, f.connection),
    /Active project changed/);
  await assert.rejects(f.importAndPlace(f.payload(""), f.connection), /no path/);
  f.project.importFiles = async () => { throw new Error("Unsupported format"); };
  await assert.rejects(f.importAndPlace(f.payload("D:\\SFX\\Boom.wav"), f.connection),
    /could not import "Boom.wav": Unsupported format/);
  assert.equal(f.commits.length, 1);
});

test("imports a Library file and opens it in the Source Monitor", async () => {
  const f = libraryFixture();
  await assert.rejects(f.importAndOpen(f.payload("D:\\SFX\\Boom.wav"), f.connection), /25\.6 or later/);
  assert.equal(f.imports.length, 0);
  const opened = [];
  f.ppro.SourceMonitor = { openProjectItem: async (item) => { opened.push(item.getId()); return true; } };
  assert.match(await f.importAndOpen(f.payload("D:\\SFX\\Boom.wav"), f.connection),
    /^Imported and opened "Boom.wav" in the Source Monitor/);
  assert.deepEqual(opened, ["boom.wav"]);
  assert.deepEqual(f.commits.map((commit) => commit.label), ["Create Spotlight Library bin"]);
});
