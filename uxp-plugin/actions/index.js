"use strict";

const { addAdjustmentLayer, registerAdjustmentLayer } = require("./adjustment-layer.js");
const { removeAttributes } = require("./remove-attributes.js");

// Commands answered with `{ ok, message }`. Marker commands return structured
// replies, so index.js routes them to markers.js through their own message type.
const handlers = new Map([
  ["add_adjustment_layer", addAdjustmentLayer],
  ["register_adjustment_layer", registerAdjustmentLayer],
  ["remove_attributes", removeAttributes]
]);

async function runAction(ppro, action, isCurrent, timeline) {
  const handler = handlers.get(action);
  if (!handler) throw new Error("This version of the Premiere bridge does not support that action.");
  return handler(ppro, isCurrent, timeline);
}

module.exports = { runAction };
