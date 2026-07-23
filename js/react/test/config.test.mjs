import assert from "node:assert/strict";
import { test } from "node:test";

import { createConfig } from "../config.mjs";

test("Nanocodex config owns worker lifecycle outside React", async () => {
  const commands = [];
  let terminated = false;
  const worker = {
    onmessage: null,
    postMessage(command) { commands.push(command); },
    terminate() { terminated = true; },
  };
  const config = createConfig({
    worker: () => worker,
    thinking: "high",
    reasoningMode: "pro",
  });
  let stateChanges = 0;
  const unsubscribe = config.subscribe(() => { stateChanges += 1; });
  const messages = [];
  config.subscribeMessages((message) => messages.push(message.type));

  const unmount = config.mount();
  assert.deepEqual(commands, [{ type: "start", thinking: "high", reasoningMode: "pro" }]);
  worker.onmessage({ data: { type: "ready" } });
  await Promise.resolve();
  assert.deepEqual(config.getSnapshot(), { status: "ready", error: undefined });
  assert.deepEqual(messages, ["ready"]);
  config.dispatch({ type: "prompt", prompt: "hello" });
  assert.deepEqual(commands.at(-1), { type: "prompt", prompt: "hello" });

  unmount();
  unsubscribe();
  assert.equal(terminated, true);
  assert.equal(config.getSnapshot().status, "idle");
  assert.throws(() => config.dispatch({ type: "prompt" }), /not running/);
  assert.ok(stateChanges >= 2);
});

test("the library requires the application to provide its Worker boundary", () => {
  assert.throws(() => createConfig(), /requires worker/);
});
