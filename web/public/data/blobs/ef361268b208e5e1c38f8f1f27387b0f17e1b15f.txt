import assert from "node:assert/strict";
import { test } from "node:test";

import { createBrowserHost } from "../browser/host.mjs";

test("browser host carries ordered frames and application tools", async () => {
  const events = [];
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    onEvent: (event) => events.push(event),
    tools: {
      double: {
        description: "Double a number.",
        parameters: { type: "object" },
        handler: ({ value }) => value * 2,
      },
    },
  });
  const connecting = host.connect("ws://example.test", "not-forwarded", "session");
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  assert.equal(JSON.parse(await connecting).status, 101);
  socket.message('{"type":"one"}');
  socket.message('{"type":"two"}');
  assert.equal(JSON.parse(await host.next(1, 10)).text, '{"type":"one"}');
  assert.equal(JSON.parse(await host.next(1, 10)).text, '{"type":"two"}');

  const execution = JSON.parse(await host.executeCode("text(await tools.double({ value: 21 }));"));
  assert.equal(execution.success, true);
  assert.match(JSON.stringify(execution.output), /42/);
  assert.equal(execution.nested_calls[0].name, "double");
  assert.equal(JSON.parse(host.toolDefinitions())[0].name, "double");
  host.emitEvent("event");
  assert.deepEqual(events, ["event"]);
});

test("browser host bounds queued receives and buffered sends", async () => {
  const host = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    maxQueuedMessages: 1,
    maxQueuedBytes: 1_024,
    maxBufferedSendBytes: 4,
  });
  const connecting = host.connect("ws://example.test", "not-forwarded", "session");
  const socket = FakeWebSocket.instances.at(-1);
  socket.open();
  await connecting;

  socket.message("first");
  socket.message("second");
  assert.match(JSON.parse(await host.next(1, 10)).detail, /receive queue exceeded/);
  assert.equal(socket.closedCode, 1009);

  const secondHost = createBrowserHost({
    WebSocketImpl: FakeWebSocket,
    maxBufferedSendBytes: 4,
  });
  const secondConnecting = secondHost.connect("ws://example.test", "not-forwarded", "session");
  const secondSocket = FakeWebSocket.instances.at(-1);
  secondSocket.open();
  await secondConnecting;
  const send = JSON.parse(await secondHost.send(1, "12345"));
  assert.equal(send.ok, false);
  assert.match(send.error, /buffered WebSocket sends exceeded/);
});

class FakeWebSocket {
  static OPEN = 1;
  static instances = [];

  constructor(url) {
    this.url = url;
    this.readyState = 0;
    this.bufferedAmount = 0;
    this.listeners = new Map();
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) || [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  open() {
    this.readyState = FakeWebSocket.OPEN;
    this.emit("open", {});
  }

  message(data) {
    this.emit("message", { data });
  }

  send() {}
  close(code) {
    this.readyState = 3;
    this.closedCode = code;
    this.emit("close", { code });
  }

  emit(type, event) {
    for (const listener of this.listeners.get(type) || []) listener(event);
  }
}
