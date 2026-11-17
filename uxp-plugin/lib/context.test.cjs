const assert = require("node:assert/strict");
const { test } = require("node:test");
const createContext = require("./context.js");
const { sourceKey } = require("../actions/adjustment-layer.js");

class VideoClipTrackItem {
  constructor(adjustmentLayer = false) { this.adjustmentLayer = adjustmentLayer; }
  async isAdjustmentLayer() { return this.adjustmentLayer; }
}
class AudioClipTrackItem {}

// A fake Premiere with an optional project, sequence and Timeline selection.
function fixture({ project = true, sequence = true, selection = [], classes = true, throws = false } = {}) {
  const activeSequence = sequence && {
    guid: "sequence-guid",
    name: "Sequence 01",
    getSelection: async () => {
      if (throws) throw new Error("selection unavailable");
      return { getTrackItems: async () => selection };
    }
  };
  const activeProject = project && {
    guid: { toString: () => "project-guid" },
    getActiveSequence: async () => activeSequence || null
  };
  const ppro = {
    Project: { getActiveProject: async () => activeProject || null },
    ...(classes ? { VideoClipTrackItem, AudioClipTrackItem } : {})
  };
  const stored = new Map();
  const storage = { getItem: (key) => stored.get(key) ?? null, setItem: (key, value) => stored.set(key, value) };
  const { readContext } = createContext(ppro, { storage, sourceKey, hostVersion: "25.6.0" });
  return { readContext, storage };
}

test("reports no project and no sequence without reading further", async () => {
  assert.deepEqual(await fixture({ project: false }).readContext(), {
    projectId: null, sequenceId: null, sequenceName: null, selection: null,
    adjustmentLayerRegistered: null, hostVersion: "25.6.0"
  });
  const noSequence = await fixture({ sequence: false }).readContext();
  assert.equal(noSequence.projectId, "project-guid");
  assert.equal(noSequence.sequenceId, null);
  assert.equal(noSequence.selection, null);
  assert.equal(noSequence.adjustmentLayerRegistered, false);
});

test("counts the selection by clip type", async () => {
  const empty = await fixture().readContext();
  assert.equal(empty.sequenceId, "sequence-guid");
  assert.equal(empty.sequenceName, "Sequence 01");
  assert.deepEqual(empty.selection, { total: 0, video: 0, audio: 0, singleAdjustmentLayer: false });

  const mixed = await fixture({
    selection: [new VideoClipTrackItem(), new VideoClipTrackItem(), new AudioClipTrackItem(), {}]
  }).readContext();
  assert.deepEqual(mixed.selection, { total: 4, video: 2, audio: 1, singleAdjustmentLayer: false });
});

test("flags a single selected adjustment layer and the registered source", async () => {
  const f = fixture({ selection: [new VideoClipTrackItem(true)] });
  const before = await f.readContext();
  assert.equal(before.selection.singleAdjustmentLayer, true);
  assert.equal(before.adjustmentLayerRegistered, false);
  f.storage.setItem(sourceKey("project-guid"), "guid:layer");
  assert.equal((await f.readContext()).adjustmentLayerRegistered, true);

  const plain = await fixture({ selection: [new VideoClipTrackItem(false)] }).readContext();
  assert.equal(plain.selection.singleAdjustmentLayer, false);
  const two = await fixture({ selection: [new VideoClipTrackItem(true), new VideoClipTrackItem(true)] }).readContext();
  assert.equal(two.selection.singleAdjustmentLayer, false);
});

test("leaves unknown facts null instead of guessing", async () => {
  const unclassified = await fixture({ classes: false, selection: [{}, {}] }).readContext();
  assert.deepEqual(unclassified.selection, { total: 2, video: null, audio: null, singleAdjustmentLayer: false });

  const single = await fixture({ classes: false, selection: [{}] }).readContext();
  assert.equal(single.selection.singleAdjustmentLayer, null);

  const failed = await fixture({ throws: true }).readContext();
  assert.equal(failed.sequenceId, "sequence-guid");
  assert.equal(failed.selection, null);
});
