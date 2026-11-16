"use strict";

const { removeAttributes } = require("./remove-attributes.js");

const handlers = new Map([
  ["remove_attributes", removeAttributes]
]);

async function runAction(ppro, action, isCurrent) {
  const handler = handlers.get(action);
  if (!handler) throw new Error("This version of the Premiere bridge does not support that action.");
  return handler(ppro, isCurrent);
}

module.exports = { runAction };
