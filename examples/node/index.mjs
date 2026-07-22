import { Agent } from "nanocodex/node";

const apiKey = process.env.OPENAI_API_KEY?.trim();
if (!apiKey) {
  throw new Error("Set OPENAI_API_KEY or put it in the repository's ignored .env file.");
}

const agent = await Agent.create({
  apiKey,
  thinking: "low",
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
const unwatch = agent.events.watch((event) => {
  if (event.type === "tool.call") console.error(`tool: ${event.payload.tool}`);
});
const turns = [];

try {
  const first = agent.turn.prompt("Use multiply to calculate 6 × 7. Return only the number.");
  turns.push(first);
  console.log("first:", await first.result());

  // Follow-on state, response IDs, and prompt-cache identity stay in the Rust agent.
  const second = agent.turn.prompt("Add one to that result. Return only the number.");
  turns.push(second);
  console.log("second:", await second.result());
} finally {
  // Long-lived applications retain the agent; short-lived scripts close the
  // Rust-owned WebSocket and driver explicitly.
  for (const turn of turns) turn.dispose();
  unwatch();
  agent.dispose();
}
