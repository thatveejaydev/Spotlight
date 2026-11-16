"use strict";

// The active project and sequence every timeline command starts from.

const NO_PROJECT = "Open a Premiere Pro project first.";
const NO_SEQUENCE = "Open a sequence in the Timeline first.";

// With `requireProject`, a missing project gets its own message instead of NO_SEQUENCE.
async function requireActiveSequence(ppro, { requireProject = false } = {}) {
  const project = await ppro.Project.getActiveProject();
  if (requireProject && !project) throw new Error(NO_PROJECT);
  const sequence = await project?.getActiveSequence();
  if (!sequence) throw new Error(NO_SEQUENCE);
  return { project, sequence };
}

module.exports = { NO_SEQUENCE, requireActiveSequence };
