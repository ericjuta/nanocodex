import init, { Nanocodex } from "../pkg-web/nanocodex.js";
import { createBrowserHost } from "./host.mjs";

globalThis.nanocodexHost = createBrowserHost({
  // Browsers cannot attach an Authorization header to WebSocket upgrades.
  // Supply an already-authorized endpoint or a custom createWebSocket here.
  createWebSocket: (endpoint) => new WebSocket(endpoint),
  onEvent: (eventJson) => postMessage({ type: "event", event: JSON.parse(eventJson) }),
  tools: {
    lookupSelection: {
      description: "Read the text currently selected in the embedding page.",
      parameters: { type: "object", additionalProperties: false },
      handler: async () => "selection supplied by the host application",
    },
  },
});

await init();

let agent;
self.onmessage = async ({ data }) => {
  if (data.type === "start") {
    agent = new Nanocodex(JSON.stringify({
      api_key: data.apiKey || "host-managed",
      websocket_url: data.websocketUrl,
      thinking: data.thinking || "medium",
    }));
    postMessage({ type: "ready" });
    return;
  }
  if (data.type === "prompt") {
    try {
      const turn = agent.prompt(data.prompt);
      postMessage({ type: "result", message: await turn.result() });
    } catch (error) {
      postMessage({ type: "error", message: error?.message || String(error) });
    }
  }
};
