"use strict";

const { requireActiveSequence } = require("../lib/active-sequence.js");

// Match names are independent of the language used in Effect Controls.
const BUILT_IN_COMPONENTS = new Set([
  "ADBE Motion",
  "ADBE Opacity",
  "ADBE Time Remapping",
  "ADBE Vector Motion",
  "ADBE Graphic Group",
  "ADBE Text",
  "ADBE Shape",
  "Internal Audio Balance",
  "Internal Audio Panner",
  "audioVolume"
]);

function isBuiltIn(matchName) {
  const name = matchName.replace(/^(?:AE|PR)\./, "");
  return BUILT_IN_COMPONENTS.has(name)
    || /^Internal (?:Channel )?Volume(?: |$)/.test(name);
}

async function prepareRemovals(clip) {
  const chain = await clip.getComponentChain();
  if (!chain || typeof chain.createRemoveComponentAction !== "function") {
    throw new Error("Premiere cannot remove effects from one of the selected clips.");
  }

  const count = chain.getComponentCount();
  const effects = [];
  // Remove from the end so earlier indices stay stable within the transaction.
  for (let index = count - 1; index >= 0; index--) {
    const component = chain.getComponentAtIndex(index);
    const matchName = await component?.getMatchName();
    if (!matchName) throw new Error("Premiere could not identify a clip component.");
    if (!isBuiltIn(matchName)) effects.push(component);
  }
  return { chain, count, effects };
}

async function removeAttributes(ppro, isCurrent) {
  const { project, sequence } = await requireActiveSequence(ppro, { requireProject: true });

  const selection = await sequence.getSelection();
  const items = await selection?.getTrackItems() || [];
  const clips = items.filter((item) => typeof item?.getComponentChain === "function");
  if (clips.length === 0) throw new Error("Select at least one timeline clip first.");

  const prepared = await Promise.all(clips.map(prepareRemovals));
  const affected = prepared.filter(({ effects }) => effects.length > 0);
  if (affected.length === 0) return "The selected clips have no applied effects.";

  let committed = false;
  project.lockedAccess(() => {
    if (!isCurrent() || prepared.some(({ chain, count }) => chain.getComponentCount() !== count)) {
      throw new Error("The timeline changed. Run Remove Attributes again.");
    }

    const removals = affected.flatMap(({ chain, effects }) =>
      effects.map((component) => chain.createRemoveComponentAction(component))
    );
    if (removals.some((action) => !action)) {
      throw new Error("Premiere could not prepare effect removal.");
    }

    committed = project.executeTransaction((transaction) => {
      for (const action of removals) {
        if (!transaction.addAction(action)) throw new Error("Premiere rejected an effect removal.");
      }
    }, "Remove Attributes");
  });
  if (!committed) throw new Error("Premiere could not remove the applied effects.");

  return affected.length === 1
    ? "Removed applied effects from 1 clip."
    : `Removed applied effects from ${affected.length} clips.`;
}

module.exports = { removeAttributes };
