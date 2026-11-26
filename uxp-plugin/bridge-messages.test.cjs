const assert = require("node:assert/strict");
const fs = require("node:fs");
const { test } = require("node:test");
const vm = require("node:vm");

const source = fs.readFileSync(require.resolve("./index.js"), "utf8");

// Loads index.js with no open project and a fake WebSocket, then connects it.
function connect() {
  const sockets = [];
  class FakeSocket {
    constructor(url) {
      this.url = url;
      this.readyState = FakeSocket.CONNECTING;
      this.sent = [];
      sockets.push(this);
    }
    send(text) { this.sent.push(JSON.parse(text)); }
    close() {}
  }
  FakeSocket.CONNECTING = 0;
  FakeSocket.OPEN = 1;
  const ppro = { Project: { getActiveProject: async () => null }, Constants: {} };
  const context = vm.createContext({
    require: (id) => {
      if (id === "premierepro") return ppro;
      if (id === "uxp") return { entrypoints: { setup() {} }, shell: {} };
      return require(id);
    },
    document: { readyState: "loading", addEventListener() {}, getElementById: () => null },
    WebSocket: FakeSocket,
    console: { ...console, warn() {} },
    setTimeout, clearTimeout, globalThis: {}
  });
  const { connectBridge } = vm.runInContext(`${source}\n({ connectBridge });`, context);
  connectBridge();
  const socket = sockets[0];
  socket.readyState = FakeSocket.OPEN;
  const settle = () => new Promise((resolve) => setImmediate(resolve));
  return {
    socket,
    async send(message) {
      await socket.onmessage({ data: JSON.stringify(message) });
      for (let i = 0; i < 5; i++) await settle();
      return socket.sent.splice(0);
    }
  };
}

test("each helper command gets its reply type, request ID and failure message", async () => {
  const bridge = connect();
  const cases = [
    [{ type: "place_project_item", requestId: 1, projectId: "p", sequenceId: "s", itemId: "i" },
      "placement_ack", /Active project changed/],
    [{ type: "open_in_source_monitor", requestId: 2, projectId: "p", itemId: "i" },
      "source_monitor_ack", /Premiere Pro 25\.6/],
    [{ type: "open_sequence", requestId: 9, projectId: "p", itemId: "i" },
      "open_sequence_ack", /Active project changed/],
    [{ type: "import_and_place", requestId: 10, projectId: "p", sequenceId: "s", path: "D:\\a.wav" },
      "placement_ack", /Active project changed/],
    [{ type: "import_and_open", requestId: 11, projectId: "p", path: "D:\\a.wav" },
      "source_monitor_ack", /Premiere Pro 25\.6/],
    [{ type: "run_action", requestId: 3, action: "not_an_action" },
      "action_ack", /does not support that action/],
    [{ type: "apply_effect", requestId: 4, name: "Blur", matchName: "blur", effectType: "videoEffect" },
      "ack", /Open a Premiere Pro project first/]
  ];
  for (const [command, type, message] of cases) {
    const [reply, ...extra] = await bridge.send(command);
    assert.deepEqual(extra, []);
    assert.equal(reply.type, type);
    assert.equal(reply.requestId, command.requestId);
    assert.equal(reply.ok, false);
    assert.match(reply.message, message);
  }

  const [preview] = await bridge.send({ type: "request_audio_preview", requestId: 5, projectId: "p", itemId: "i" });
  assert.equal(preview.type, "audio_preview_ack");
  assert.equal(preview.requestId, 5);
  assert.equal(preview.source, null);
  assert.match(preview.message, /Active project changed/);

  const [marker] = await bridge.send({ type: "marker_command", requestId: 6, operation: "list" });
  assert.equal(marker.type, "marker_ack");
  assert.equal(marker.requestId, 6);
  assert.equal(marker.result, null);
  assert.ok(marker.message);

  const items = await bridge.send({ type: "request_project_items", requestId: 7 });
  assert.ok(items.some((reply) => reply.type === "project_items" && reply.requestId === 7));

  const [context] = await bridge.send({ type: "request_context", requestId: 8 });
  assert.equal(context.type, "context");
  assert.equal(context.requestId, 8);
  assert.deepEqual(context.context, {
    projectId: null, sequenceId: null, sequenceName: null, selection: null,
    adjustmentLayerRegistered: null, hostVersion: null
  });

  const [catalog] = await bridge.send({ type: "request_catalog" });
  assert.equal(catalog.type, "catalog");
  assert.ok(Array.isArray(catalog.effects));
});

test("commands without a request ID and unknown types get no reply", async () => {
  const bridge = connect();
  for (const type of ["place_project_item", "open_in_source_monitor", "run_action",
    "request_audio_preview", "marker_command", "request_project_items", "request_context", "open_sequence"]) {
    assert.deepEqual(await bridge.send({ type, requestId: "1" }), [], type);
  }
  assert.deepEqual(await bridge.send({ type: "toString", requestId: 1 }), []);
  assert.deepEqual(await bridge.send({ type: "something_new", requestId: 1 }), []);
});

test("a second placement waits for the first to finish", async () => {
  const bridge = connect();
  const first = bridge.socket.onmessage({ data: JSON.stringify(
    { type: "place_project_item", requestId: 1, projectId: "p", sequenceId: "s", itemId: "i" }) });
  const replies = await bridge.send({ type: "place_project_item", requestId: 2, projectId: "p", sequenceId: "s", itemId: "i" });
  await first;
  replies.push(...bridge.socket.sent.splice(0));
  const byId = new Map(replies.map((reply) => [reply.requestId, reply]));
  assert.equal(replies.length, 2);
  assert.match(byId.get(2).message, /already in progress/);
  assert.match(byId.get(1).message, /Active project changed/);
});
