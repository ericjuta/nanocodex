import assert from "node:assert/strict";
import { test } from "node:test";

import { createNanocodexConfig } from "../config.mjs";

test("Nanocodex config owns worker lifecycle outside React", async () => {
  const commands = [];
  let terminated = false;
  const worker = {
    onmessage: null,
    postMessage(command) { commands.push(command); },
    terminate() { terminated = true; },
  };
  const config = createNanocodexConfig({
    thinking: "high",
    reasoningMode: "pro",
    createWorker: () => worker,
    checkHealth: async () => ({ agent_configured: true, credential_source: "user" }),
  });
  let stateChanges = 0;
  const unsubscribe = config.subscribe(() => { stateChanges += 1; });
  const messages = [];
  config.subscribeMessages((message) => messages.push(message.type));

  const unmount = config.mount();
  assert.deepEqual(commands, [{ type: "start", thinking: "high", reasoningMode: "pro" }]);
  worker.onmessage({ data: { type: "ready" } });
  await Promise.resolve();
  assert.deepEqual(config.getState(), {
    ready: true,
    configured: true,
    credentialSource: "user",
    stopped: false,
  });
  assert.deepEqual(messages, ["ready"]);

  unmount();
  unsubscribe();
  assert.equal(terminated, true);
  assert.equal(config.getState().ready, false);
  assert.ok(stateChanges >= 2);
});

test("the library requires the application to provide its Worker boundary", () => {
  assert.throws(() => createNanocodexConfig(), /requires createWorker/);
});
