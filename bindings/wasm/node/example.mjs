import { createRequire } from "node:module";

import { createNodeHost } from "./host.mjs";

const require = createRequire(import.meta.url);
globalThis.nanocodexHost = createNodeHost({
  onEvent(eventJson) {
    const event = JSON.parse(eventJson);
    if (event.type === "tool.call") console.error(`tool: ${event.payload.tool}`);
  },
  tools: {
    multiply: {
      description: "Multiply two integers.",
      parameters: {
        type: "object",
        properties: {
          left: { type: "integer" },
          right: { type: "integer" },
        },
        required: ["left", "right"],
        additionalProperties: false,
      },
      handler: ({ left, right }) => left * right,
    },
  },
});

const { Nanocodex } = require("../pkg-node/nanocodex.js");
const agent = new Nanocodex(JSON.stringify({
  api_key: process.env.OPENAI_API_KEY || "",
  thinking: "low",
}));

const first = agent.prompt("Use multiply to calculate 6 × 7. Reply with only the result.");
console.log("first:", await first.result());

// Follow-on context is retained inside the same Rust/WASM agent.
const second = agent.prompt("Add one to that result. Reply with only the number.");
console.log("second:", await second.result());
