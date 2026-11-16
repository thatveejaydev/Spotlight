"use strict";

function sequenceRef(project, sequence) {
  const projectId = project.guid?.toString();
  const sequenceId = sequence.guid?.toString();
  if (!projectId || !sequenceId) throw new Error("Premiere could not identify the active sequence.");
  return { projectId, sequenceId };
}

function sameSequence(left, right) {
  return left.projectId === right?.projectId && left.sequenceId === right?.sequenceId;
}

function ticks(time) {
  const value = time?.ticks?.toString();
  if (!/^-?\d+$/.test(value ?? "")) throw new Error("Premiere returned an invalid marker position.");
  return BigInt(value).toString();
}

async function activeSequence(ppro, expected) {
  const project = await ppro.Project.getActiveProject();
  const sequence = await project?.getActiveSequence();
  if (!sequence) throw new Error("Open a sequence in the Timeline first.");
  const ref = sequenceRef(project, sequence);
  if (expected && !sameSequence(ref, expected)) {
    throw new Error("The active sequence changed. Open the action again.");
  }
  return { project, sequence, ref };
}

async function activeTimeline(ppro, expected) {
  const timeline = await activeSequence(ppro, expected);
  return { ...timeline, markers: await ppro.Markers.getMarkers(timeline.sequence) };
}

function readMarker(marker) {
  return {
    name: marker.getName() ?? "",
    comments: marker.getComments() ?? "",
    ticks: ticks(marker.getStart())
  };
}

function ensureConnected(isCurrent) {
  if (!isCurrent()) throw new Error("The Premiere bridge disconnected.");
}

async function prepareMarker(ppro, isCurrent) {
  const { project, sequence, ref, markers } = await activeTimeline(ppro);
  const position = await sequence.getPlayerPosition();
  const location = { sequence: ref, ticks: ticks(position) };
  // Give a playing timeline time to advance before creating anything at a captured frame.
  await new Promise((resolve) => setTimeout(resolve, 100));
  await activeSequence(ppro, ref);
  if (ticks(await sequence.getPlayerPosition()) !== location.ticks) {
    throw new Error("Pause playback before running Add Marker.");
  }
  ensureConnected(isCurrent);
  const selection = await sequence.getSelection();
  if ((await selection?.getTrackItems())?.length && !await sequence.clearSelection()) {
    throw new Error("Premiere could not clear the Timeline selection.");
  }
  const current = await activeSequence(ppro, ref);
  await checkMarkerContext(current.sequence, location, isCurrent);

  let created = false;
  project.lockedAccess(() => {
    ensureConnected(isCurrent);
    if (markers.getMarkers().some((marker) => ticks(marker.getStart()) === location.ticks)) return;
    const action = markers.createAddMarkerAction(
      "", "Comment", position, ppro.TickTime.createWithTicks("0"), ""
    );
    if (!action || !project.executeTransaction((transaction) => {
      if (!transaction.addAction(action)) throw new Error("Premiere rejected the marker.");
    }, "Add Marker")) {
      throw new Error("Premiere could not create the marker.");
    }
    created = true;
  });
  return { kind: "prepared", location, created };
}

async function checkMarkerContext(sequence, location, isCurrent) {
  const selection = await sequence.getSelection();
  const trackItems = await selection?.getTrackItems();
  const position = await sequence.getPlayerPosition();
  ensureConnected(isCurrent);
  if (trackItems?.length || ticks(position) !== location.ticks) {
    throw new Error("The playhead or Timeline selection changed. Run Add Marker again.");
  }
}

async function activeProject(ppro, expectedProjectId) {
  const project = await ppro.Project.getActiveProject();
  const projectId = project?.guid?.toString();
  if (!projectId) throw new Error("Open a Premiere project first.");
  if (expectedProjectId !== undefined && projectId !== expectedProjectId) {
    throw new Error("The active project changed. Open Go to Marker again.");
  }
  return { project, projectId };
}

async function listProjectMarkers(ppro, isCurrent) {
  const { project, projectId } = await activeProject(ppro);
  const entries = [];
  for (const sequence of await project.getSequences()) {
    ensureConnected(isCurrent);
    const sequenceId = sequence.guid?.toString();
    if (!sequenceId) continue;
    const sequenceName = sequence.name || "Unnamed sequence";
    const markers = await ppro.Markers.getMarkers(sequence);
    for (const marker of markers.getMarkers()) {
      entries.push({ ...readMarker(marker), sequenceId, sequenceName });
    }
  }
  await activeProject(ppro, projectId);
  ensureConnected(isCurrent);
  return { kind: "list", projectId, projectName: project.name ?? "", markers: entries };
}

async function seekProjectMarker(ppro, command, isCurrent) {
  const { project, projectId } = await activeProject(ppro, command.projectId);
  const target = command.marker;
  const sequence = await project.getSequence(ppro.Guid.fromString(target.sequenceId));
  if (!sequence) throw new Error("That sequence was removed. Open Go to Marker again.");

  const markers = await ppro.Markers.getMarkers(sequence);
  const marker = markers.getMarkers().find((item) => {
    const entry = readMarker(item);
    return entry.ticks === target.ticks && entry.name === target.name && entry.comments === target.comments;
  });
  if (!marker) throw new Error("That marker changed or was removed. Open Go to Marker again.");

  const active = await project.getActiveSequence();
  await activeProject(ppro, projectId);
  ensureConnected(isCurrent);
  if (active?.guid?.toString() !== target.sequenceId) {
    if (!await project.openSequence(sequence)) throw new Error("Premiere could not open the marker's sequence.");
    if ((await project.getActiveSequence())?.guid?.toString() !== target.sequenceId) {
      ensureConnected(isCurrent);
      if (!await project.setActiveSequence(sequence)) {
        throw new Error("Premiere could not activate the marker's sequence.");
      }
    }
    await activeProject(ppro, projectId);
    ensureConnected(isCurrent);
  }
  if (!await sequence.setPlayerPosition(ppro.TickTime.createWithTicks(target.ticks))) {
    throw new Error("Premiere could not move the playhead.");
  }
  return { kind: "sought" };
}

async function runMarkerCommand(ppro, command, isCurrent) {
  switch (command.operation) {
    case "list":
      return listProjectMarkers(ppro, isCurrent);
    case "seek":
      return seekProjectMarker(ppro, command, isCurrent);
    case "prepare":
      return prepareMarker(ppro, isCurrent);
    case "confirm": {
      const { markers } = await activeTimeline(ppro, command.location.sequence);
      const { sequence } = await activeSequence(ppro, command.location.sequence);
      await checkMarkerContext(sequence, command.location, isCurrent);
      if (!markers.getMarkers().some((marker) => ticks(marker.getStart()) === command.location.ticks)) {
        throw new Error("The marker is no longer at the playhead.");
      }
      return { kind: "confirmed" };
    }
    default:
      throw new Error("This version of the Premiere bridge does not support that marker command.");
  }
}

module.exports = { runMarkerCommand };
