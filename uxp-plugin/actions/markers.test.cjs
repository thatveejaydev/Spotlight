const assert = require("node:assert/strict");
const { test } = require("node:test");
const { runMarkerCommand } = require("./markers.js");

function fixture() {
  const transactions = [];
  const jumps = [];
  const opened = [];
  const markers = [];
  let locked = false;
  const sequence = {
    guid: "sequence",
    name: "Selects",
    position: "9007199254740993",
    selection: [{}],
    getPlayerPosition() { return { ticks: this.position }; },
    getSelection() { return { getTrackItems: () => this.selection }; },
    clearSelection() { this.selection = []; return true; },
    setPlayerPosition(time) { jumps.push(time); return true; },
  };
  const nestedMarkers = [];
  const nested = {
    ...sequence,
    guid: "nested",
    name: "Nested",
    setPlayerPosition(time) { jumps.push({ nested: time }); return true; },
  };
  const project = {
    guid: "project",
    name: "Editing project",
    sequence,
    getActiveSequence() { return this.sequence; },
    getSequences() { return [sequence, nested]; },
    getSequence(id) { return this.getSequences().find((entry) => entry.guid === id); },
    openSequence(target) { opened.push(target.guid); this.sequence = target; return true; },
    lockedAccess(callback) {
      locked = true;
      try { callback(); } finally { locked = false; }
    },
    executeTransaction(callback, label) {
      assert.ok(locked);
      const actions = [];
      callback({ addAction(action) { actions.push(action); return true; } });
      actions.forEach((action) => action());
      transactions.push(label);
      return true;
    },
  };
  function marker(name, start, comments = "") {
    return {
      getName: () => name, getComments: () => comments,
      getStart: () => ({ ticks: start }),
      getDuration: () => ({ ticks: "254016000000" }),
    };
  }
  const ppro = {
    Project: { getActiveProject: () => project },
    TickTime: { createWithTicks: (ticks) => ({ ticks }) },
    Guid: { fromString: (id) => id },
    Markers: { getMarkers: (owner) => {
      assert.ok(owner === sequence || owner === nested);
      return {
        getMarkers: () => owner === nested ? nestedMarkers : markers,
        createAddMarkerAction(name, type, start, duration, comments) {
          assert.ok(locked);
          assert.equal(type, "Comment");
          assert.equal(duration.ticks, "0");
          return () => markers.push(marker(name, start.ticks, comments));
        },
      };
    } },
  };
  return {
    run: (operation, fields = {}, isCurrent = () => true) =>
      runMarkerCommand(ppro, { operation, ...fields }, isCurrent),
    markers, nestedMarkers, marker, sequence, project, transactions, jumps, opened,
  };
}

test("prepares one sequence marker at the playhead and confirms its exact location", async () => {
  const f = fixture();
  const result = await f.run("prepare");
  assert.deepEqual(result, {
    kind: "prepared", created: true,
    location: { sequence: { projectId: "project", sequenceId: "sequence" }, ticks: f.sequence.position },
  });
  assert.deepEqual(f.sequence.selection, []);
  assert.deepEqual(f.transactions, ["Add Marker"]);
  assert.equal(f.markers.length, 1);
  assert.deepEqual(await f.run("confirm", { location: result.location }), { kind: "confirmed" });
  assert.equal((await f.run("prepare")).created, false);
  assert.deepEqual(f.transactions, ["Add Marker"]);

  f.sequence.position = "9007199254740994";
  await assert.rejects(f.run("confirm", { location: result.location }), /playhead.*changed/);
  assert.equal(f.markers.length, 1);
});

test("uses exact range starts and rejects moved markers, other projects and stale connections", async () => {
  const f = fixture();
  f.markers.push(f.marker("Cue", "9007199254740993", "first"), f.marker("Cue", "9007199254740994", "second"), f.marker("", "0"));
  const result = await f.run("list");
  assert.equal(result.markers[2].name, "");
  assert.equal(result.markers[2].sequenceName, "Selects");
  assert.equal(result.projectName, "Editing project");
  const target = { projectId: result.projectId, marker: result.markers[1] };
  await f.run("seek", target);
  assert.deepEqual(f.jumps, [{ ticks: "9007199254740994" }]);

  await assert.rejects(f.run("seek", target, () => false), /disconnected/);
  f.markers[1] = f.marker("Cue", "9007199254740995", "second");
  await assert.rejects(f.run("seek", target), /changed or was removed/);
  f.project.guid = "other-project";
  await assert.rejects(f.run("seek", target), /active project changed/);
  assert.equal(f.jumps.length, 1);
});

test("lists markers even without an active timeline and opens the owning nest on selection", async () => {
  const f = fixture();
  f.project.sequence = null;
  f.markers.push(f.marker("Cue", "254016000000"));
  f.nestedMarkers.push(f.marker("Cue", "508032000000"));
  const result = await f.run("list");
  assert.deepEqual(result.markers.map(({ sequenceName, sequenceId }) => [sequenceName, sequenceId]), [
    ["Selects", "sequence"], ["Nested", "nested"]
  ]);

  await f.run("seek", { projectId: result.projectId, marker: result.markers[1] });
  assert.deepEqual(f.opened, ["nested"]);
  assert.deepEqual(f.jumps, [{ nested: { ticks: "508032000000" } }]);
});

test("leaves the timeline alone when the playhead advances during Add Marker", async () => {
  const f = fixture();
  let positionReads = 0;
  f.sequence.getPlayerPosition = () => ({ ticks: String(++positionReads) });
  await assert.rejects(f.run("prepare"), /Pause playback/);
  assert.equal(f.sequence.selection.length, 1);
  assert.deepEqual(f.transactions, []);
});

test("reports an empty marker list and avoids mutations when the connection is stale", async () => {
  const f = fixture();
  assert.deepEqual((await f.run("list")).markers, []);
  await assert.rejects(f.run("prepare", {}, () => false), /disconnected/);
  assert.equal(f.sequence.selection.length, 1);
  assert.deepEqual(f.transactions, []);
});

test("adds a named marker at the playhead without touching the selection", async () => {
  const f = fixture();
  assert.deepEqual(await f.run("add", { name: "  Intro  " }), { kind: "added" });
  assert.equal(f.markers.length, 1);
  assert.equal(f.markers[0].getName(), "Intro");
  assert.equal(f.markers[0].getStart().ticks, f.sequence.position);
  assert.deepEqual(f.transactions, ['Add Marker "Intro"']);
  assert.equal(f.sequence.selection.length, 1);

  await assert.rejects(f.run("add", { name: "Again" }), /already exists at the playhead/);
  assert.equal(f.markers.length, 1);

  f.sequence.position = "0";
  assert.deepEqual(await f.run("add", { name: 42 }), { kind: "added" });
  assert.equal(f.markers[1].getName(), "");
  assert.deepEqual(f.transactions.at(-1), "Add Marker");
  assert.equal(f.markers.length, 2);
});

test("a named marker needs a paused timeline and a connected bridge", async () => {
  const f = fixture();
  const getPlayerPosition = f.sequence.getPlayerPosition;
  let reads = 0;
  f.sequence.getPlayerPosition = function () {
    reads += 1;
    return reads === 1 ? getPlayerPosition.call(this) : { ticks: "1" };
  };
  await assert.rejects(f.run("add", { name: "Moving" }), /Pause playback/);
  f.sequence.getPlayerPosition = getPlayerPosition;
  await assert.rejects(f.run("add", { name: "Gone" }, () => false), /disconnected/);
  assert.equal(f.markers.length, 0);
});
