"use strict";

// Small helpers shared by the bridge modules. No Premiere access.

function nowMs() {
  return typeof performance !== "undefined" && typeof performance.now === "function"
    ? performance.now()
    : Date.now();
}

async function mapWithConcurrency(items, limit, worker) {
  if (!Array.isArray(items) || items.length === 0) return [];
  const concurrency = Math.max(1, Math.min(limit, items.length));
  const results = new Array(items.length);
  let nextIndex = 0;
  await Promise.all(
    Array.from({ length: concurrency }, async () => {
      while (nextIndex < items.length) {
        const index = nextIndex++;
        results[index] = await worker(items[index], index);
      }
    })
  );
  return results;
}

function guid(object) {
  return object?.guid?.toString() || null;
}

module.exports = { nowMs, mapWithConcurrency, guid };
