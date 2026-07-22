import { createRequire } from "node:module";

import { createNodeHost } from "../../bindings/wasm/node/host.mjs";

const require = createRequire(import.meta.url);
const { Nanocodex } = require("../../bindings/wasm/pkg-node/nanocodex.js");

globalThis.nanocodexHost = createNodeHost({
  onEvent: (eventJson) => {
    const event = JSON.parse(eventJson);
    if (event.type === "tool_call") console.error(`tool: ${event.payload.tool}`);
  },
  tools: {
    multiply: {
      description: "Multiply two numbers.",
      parameters: {
        type: "object",
        properties: { left: { type: "number" }, right: { type: "number" } },
        required: ["left", "right"],
        additionalProperties: false,
      },
      handler: async ({ left, right }) => left * right,
    },
  },
});

const agent = new Nanocodex(JSON.stringify({
  api_key: process.env.OPENAI_API_KEY,
  thinking: "low",
}));

const first = agent.prompt("Use multiply to calculate 6 × 7. Return only the number.");
console.log("first:", await first.result());

// Follow-on state, response IDs, and prompt-cache identity stay in the Rust agent.
const second = agent.prompt("Add one to that result. Return only the number.");
console.log("second:", await second.result());

// Release the owned Rust session so its WebSocket and background driver close
// before this short-lived example exits. Long-lived applications keep it.
agent.free();
