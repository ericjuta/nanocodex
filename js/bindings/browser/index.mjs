import init, { Nanocodex } from "../pkg-web/nanocodex.js";
import { Engine, createEventChannel, toWasmConfig } from "../index.mjs";
import { createBrowserHost } from "./host.mjs";

let initialized;

export function browser(options = {}) {
  const {
    apiKey = "host-managed",
    websocketUrl,
    apiBaseUrl,
    module,
    onEvent,
    ...hostOptions
  } = options;
  const events = createEventChannel(onEvent);
  const host = createBrowserHost({ ...hostOptions, onEvent: events.emit });

  return Engine.from({
    key: "browser-wasm",
    name: "Nanocodex Browser WASM",
    type: "browser",
    async create(config) {
      installHost(host);
      initialized ||= module === undefined
        ? init()
        : init({ module_or_path: module });
      await initialized;
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
