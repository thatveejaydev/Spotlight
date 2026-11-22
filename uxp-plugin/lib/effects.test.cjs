const assert = require("node:assert/strict");
const { test } = require("node:test");
const createEffects = require("./effects.js");

class VideoClipTrackItem {
  getComponentChain() {
    return { createAppendComponentAction: (component) => component };
  }
  createAddVideoTransitionAction(transition) {
    return transition;
  }
  getStartTime() { return { ticks: "0" }; }
  getEndTime() { return { ticks: "100" }; }
}

// Returns the match names Premiere was asked to apply.
function fixture() {
  const applied = [];
  const selected = [new VideoClipTrackItem()];
  const sequence = { getSelection: () => ({ getTrackItems: () => selected }) };
  const project = {
    getActiveSequence: () => sequence,
    lockedAccess: (callback) => callback(),
    executeTransaction(callback) {
      callback({ addAction(action) { applied.push(action); return true; } });
      return true;
    }
  };
  const effects = createEffects({
    VideoClipTrackItem,
    Project: { getActiveProject: () => project },
    VideoFilterFactory: { createComponent: (matchName) => matchName },
    TransitionFactory: { createVideoTransition: (matchName) => matchName }
  });
  return { applied, apply: effects.applyEffectToSelection };
}

const entry = (name, matchName, effectType = "videoEffect") => ({ name, matchName, effectType });

test("the picked match name wins over an earlier effect with the same display name", async () => {
  for (const [type, name, first, picked] of [
    ["videoEffect", "Glow", "VendorA.Glow", "VendorB.Glow"],
    ["videoTransition", "Cross Dissolve", "ADBE Cross Dissolve", "AE.ADBE Cross Dissolve New"]
  ]) {
    const catalog = [entry(name, first, type), entry(name, picked, type)];
    for (const requested of [picked, picked.toLowerCase()]) {
      const f = fixture();
      await f.apply(entry(name, requested, type), catalog);
      assert.deepEqual(f.applied, [picked]);
    }
  }
});

test("a match name wins over another effect's display name", async () => {
  const f = fixture();
  const catalog = [entry("Glow", "Other.Effect"), entry("Soft Glow", "Vendor.Glow")];
  await f.apply(entry("Glow", "Vendor.Glow"), catalog);
  assert.deepEqual(f.applied, ["Vendor.Glow"]);
});

test("a built-in entry with an outdated match name still resolves by its name", async () => {
  for (const name of ["Tint", "tint"]) {
    const f = fixture();
    await f.apply(entry(name, "outdated"), [entry("Tint", "AE.ADBE Tint")]);
    assert.deepEqual(f.applied, ["AE.ADBE Tint"]);
  }
});

test("an unknown match name never picks between effects that share a name", async () => {
  const f = fixture();
  const catalog = [entry("Glow", "VendorA.Glow"), entry("Glow", "VendorB.Glow")];
  await assert.rejects(f.apply(entry("Glow", "outdated"), catalog), /More than one effect is named "Glow"/);
  assert.deepEqual(f.applied, []);
});
