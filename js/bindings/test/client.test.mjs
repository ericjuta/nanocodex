import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions, Agent, Engine } from "../index.mjs";

test("the headless client exposes matching standalone and decorated actions", async () => {
  const events = new Set();
  const engine = Engine.from({
    key: "test",
    name: "Test engine",
    create: () => rawAgent("session-1"),
    subscribe(listener) {
      events.add(listener);
      return () => events.delete(listener);
    },
  });
  const agent = await Agent.create({ engine, apiKey: "test" });

  const first = agent.turn.prompt({ input: "first" });
  assert.equal(await first.result(), "session-1:first");
  const second = Actions.turn.prompt(agent, "second");
  assert.equal(await Actions.turn.result(second), "session-1:second");

  const seen = [];
  const unwatch = agent.events.watch((event) => seen.push(event.type));
  for (const listener of events) {
    listener({ type: "ignored", request_id: "another-session" });
    listener({ type: "accepted", request_id: "session-1" });
  }
  unwatch();
  assert.deepEqual(seen, ["accepted"]);

  const branch = await agent.fork.from({ turn: first });
  assert.equal(branch.sessionId, "session-1-fork");
  assert.equal(await branch.turn.prompt("branch").result(), "session-1-fork:branch");

  const extended = agent.extend(() => ({ inspect: { session: () => agent.sessionId } }));
  assert.equal(extended.inspect.session(), "session-1");
});

function rawAgent(sessionId) {
  return {
    sessionId,
    prompt(input) {
      return rawTurn(`${sessionId}:${input}`);
    },
    promptContent(input) {
      return rawTurn(`${sessionId}:${JSON.parse(input)[0].text}`);
    },
    async fork() {
      return rawAgent(`${sessionId}-fork`);
    },
    async forkFrom() {
      return rawAgent(`${sessionId}-fork`);
    },
    async spawn() {
      return rawAgent(`${sessionId}-spawn`);
    },
    free() {},
  };
}

function rawTurn(value) {
  return {
    async result() { return value; },
    async steer() {},
    async steerContent() {},
    async cancel() {},
    free() {},
  };
}
