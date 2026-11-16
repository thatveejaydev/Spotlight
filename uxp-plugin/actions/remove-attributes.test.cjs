const assert = require("node:assert/strict");
const { test } = require("node:test");
const { runAction } = require("./index.js");

function fixture() {
  let lockHeld = false;
  const transactions = [];
  const selection = [];
  const project = {
    getActiveSequence: async () => ({
      getSelection: async () => ({ getTrackItems: async () => selection })
    }),
    lockedAccess(callback) {
      lockHeld = true;
      try { callback(); } finally { lockHeld = false; }
    },
    executeTransaction(callback, label) {
      assert.ok(lockHeld);
      const actions = [];
      callback({ addAction(action) { actions.push(action); return true; } });
      for (const action of actions) action();
      transactions.push(label);
      return true;
    }
  };
  const ppro = { Project: { getActiveProject: async () => project } };

  function clip(names) {
    const components = names.map((name) => ({ name, getMatchName: async () => name }));
    const chain = {
      getComponentCount: () => components.length,
      getComponentAtIndex: (index) => components[index],
      createRemoveComponentAction(component) {
        assert.ok(lockHeld);
        return () => components.splice(components.indexOf(component), 1);
      }
    };
    return {
      chain,
      getComponentChain: async () => chain,
      names: () => components.map(({ name }) => name)
    };
  }

  return {
    selection, transactions, clip,
    remove: (isCurrent = () => true) => runAction(ppro, "remove_attributes", isCurrent)
  };
}

test("removes duplicate video and audio effects from the selection in one transaction", async () => {
  const f = fixture();
  const video = f.clip([
    "AE.ADBE Motion", "AE.ADBE Opacity", "AE.ADBE Time Remapping",
    "AE.ADBE Text", "AE.ADBE Crop", "AE.ADBE Crop"
  ]);
  const audio = f.clip(["Internal Volume Stereo", "Internal Channel Volume Stereo", "Audio Filter"]);
  const unselected = f.clip(["AE.ADBE Crop"]);
  f.selection.push(video, {}, audio);

  assert.equal(await f.remove(), "Removed applied effects from 2 clips.");
  assert.deepEqual(f.transactions, ["Remove Attributes"]);
  assert.deepEqual(video.names(), ["AE.ADBE Motion", "AE.ADBE Opacity", "AE.ADBE Time Remapping", "AE.ADBE Text"]);
  assert.deepEqual(audio.names(), ["Internal Volume Stereo", "Internal Channel Volume Stereo"]);
  assert.deepEqual(unselected.names(), ["AE.ADBE Crop"]);
  assert.equal(await f.remove(), "The selected clips have no applied effects.");
  assert.equal(f.transactions.length, 1);
});

test("rejects an empty selection, stale timeline, or unidentifiable component before committing", async () => {
  const f = fixture();
  await assert.rejects(f.remove(), /Select at least one timeline clip/);
  const clip = f.clip(["AE.ADBE Crop"]);
  f.selection.push(clip);
  await assert.rejects(f.remove(() => false), /timeline changed/);
  f.selection.push(f.clip([""]));
  await assert.rejects(f.remove(), /could not identify/);
  assert.deepEqual(clip.names(), ["AE.ADBE Crop"]);
  assert.deepEqual(f.transactions, []);
});
