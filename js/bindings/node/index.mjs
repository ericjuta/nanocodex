import { createRequire } from "node:module";

import { Agent as CoreAgent, Engine, createEventChannel, toWasmConfig } from "../index.mjs";
import { createNodeHost } from "./host.mjs";

const require = createRequire(import.meta.url);
const { Nanocodex } = require("../pkg-node/nanocodex.js");

export const Agent = Object.freeze({
  create(options = {}) {
    const {
      thinking,
      reasoningMode,
      instructions,
      sessionId,
      ...engineOptions
    } = options;
    return CoreAgent.create({
      engine: node(engineOptions),
      thinking,
      reasoningMode,
      instructions,
      sessionId,
    });
  },
});

export function node(options = {}) {
  const {
    apiKey,
    websocketUrl,
    apiBaseUrl,
    onEvent,
    ...hostOptions
  } = options;
  const events = createEventChannel(onEvent);
  const host = createNodeHost({ ...hostOptions, onEvent: events.emit });

  return Engine.from({
    key: "node-wasm",
    name: "Nanocodex Node WASM",
    type: "node",
    create(config) {
      installHost(host);
      return new Nanocodex(JSON.stringify(toWasmConfig({
        apiKey,
        websocketUrl,
        apiBaseUrl,
        ...config,
      })));
    },
    subscribe: events.subscribe,
  });
}

function installHost(host) {
  globalThis.nanocodexHost = host;
}
