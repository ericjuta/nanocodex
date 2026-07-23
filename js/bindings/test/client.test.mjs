import assert from "node:assert/strict";
import { test } from "node:test";

import { Actions } from "../index.mjs";
import { createAgentClient, defineRuntime } from "../internal.mjs";

test("the headless client exposes matching direct and standalone actions", async () => {
  const events = new Set();
  const runtime = defineRuntime({
    create: () => rawAgent("session-1"),
    subscribe(listener) {
      events.add(listener);
      return () => events.delete(listener);
    },
    decorate: (agent) => agent.extend(Actions.agentActions()),
  });
  const agent = await createAgentClient(runtime);

  const first = agent.turn.prompt({ input: "first" });
  assert.equal(await first.result(), "session-1:first");
  const second = Actions.turn.prompt(agent, { input: "second" });
  assert.equal(await Actions.turn.getResult(second), "session-1:second");

  const seen = [];
  const watch = agent.events.watch();
  const unwatch = watch.onEvent((event) => seen.push(event.type));
  for (const listener of events) {
    listener({ type: "ignored", request_id: "another-session" });
    listener({ type: "accepted", request_id: "session-1" });
  }
  unwatch();
  watch.off();
  assert.deepEqual(seen, ["accepted"]);

  const iterable = Actions.events.watch(agent);
  const iterator = iterable[Symbol.asyncIterator]();
  const next = iterator.next();
  for (const listener of events) listener({ type: "streamed", request_id: "session-1" });
  assert.deepEqual(await next, {
    done: false,
    value: { type: "streamed", request_id: "session-1" },
  });
  await iterator.return();
  iterable.off();

  const branch = await agent.session.fork({ at: first });
  assert.equal(branch.sessionId, "session-1-fork");
  assert.equal(await branch.turn.prompt({ input: "branch" }).result(), "session-1-fork:branch");

  const fresh = await agent.session.spawn();
  assert.equal(fresh.sessionId, "session-1-spawn");

  const extended = agent.extend((client) => ({ inspect: { session: () => client.sessionId } }));
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
