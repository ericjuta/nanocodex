import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import WebSocket, { WebSocketServer } from "ws";

import { createBrowserHost } from "../browser/host.mjs";
import init, { Nanocodex } from "../pkg-web/nanocodex.js";

test("web-target WASM runs the shared model loop through the browser host", async () => {
  const server = new WebSocketServer({ host: "127.0.0.1", port: 0 });
  await new Promise((resolve, reject) => {
    server.once("listening", resolve);
    server.once("error", reject);
  });
  const connection = new Promise((resolve) => server.once("connection", resolve));
  const events = [];
  globalThis.nanocodexHost = createBrowserHost({
    WebSocketImpl: WebSocket,
    onEvent: (eventJson) => events.push(JSON.parse(eventJson)),
  });
  const wasm = await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url));
  await init({ module_or_path: wasm });

  const scenario = (async () => {
    const socket = await connection;
    const reader = messageReader(socket);
    await reader.next();
    send(socket, { type: "response.completed", response: { id: "web-warmup", usage: null } });
    const generation = await reader.next();
    assert.equal(generation.previous_response_id, "web-warmup");
    send(socket, {
      type: "response.completed",
      response: {
        id: "web-final",
        status: "completed",
        output: [{
          type: "message",
          role: "assistant",
          content: [{ type: "output_text", text: "WEB_WASM_OK" }],
        }],
        usage: null,
      },
    });
  })();

  const endpoint = `ws://127.0.0.1:${server.address().port}`;
  const agent = new Nanocodex(JSON.stringify({
    api_key: "host-managed",
    thinking: "low",
    websocket_url: endpoint,
    session_id: "web-session",
  }));
  assert.equal(await agent.prompt("Reply with WEB_WASM_OK.").result(), "WEB_WASM_OK");
  await scenario;
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(events.filter((event) => event.type === "run.completed").length, 1);

  for (const socket of server.clients) socket.terminate();
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
});

function messageReader(socket) {
  const messages = [];
  let waiter;
  socket.on("message", (data) => {
    const value = JSON.parse(data.toString("utf8"));
    if (waiter) {
      const resolve = waiter;
      waiter = undefined;
      resolve(value);
    } else {
      messages.push(value);
    }
  });
  return {
    next() {
      if (messages.length) return Promise.resolve(messages.shift());
      return new Promise((resolve) => { waiter = resolve; });
    },
  };
}

function send(socket, value) {
  socket.send(JSON.stringify(value));
}
