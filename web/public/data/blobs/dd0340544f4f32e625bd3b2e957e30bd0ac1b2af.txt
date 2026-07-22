import assert from "node:assert/strict";
import { test } from "node:test";

import { upstreamHeaders, validateWebSocketRequest } from "./protocol.mjs";

test("the Worker adds the secret and stable session headers upstream", () => {
  const headers = upstreamHeaders("worker-secret", "browser-session");
  assert.equal(headers.Authorization, "Bearer worker-secret");
  assert.equal(headers.Upgrade, "websocket");
  assert.equal(headers["OpenAI-Beta"], "responses_websockets=2026-02-06");
  assert.equal(headers["x-openai-internal-codex-responses-lite"], "true");
  assert.equal(headers["session-id"], "browser-session");
  assert.equal(headers["thread-id"], "browser-session");
});

test("the Worker accepts only same-origin WebSocket upgrades with valid sessions", () => {
  const accepted = new Request("https://app.example/api/responses?session_id=session-1", {
    headers: { Origin: "https://app.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(accepted), undefined);

  const crossOrigin = new Request("https://app.example/api/responses?session_id=session-1", {
    headers: { Origin: "https://other.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(crossOrigin)?.status, 403);

  const invalidSession = new Request("https://app.example/api/responses?session_id=bad%20session", {
    headers: { Origin: "https://app.example", Upgrade: "websocket" },
  });
  assert.equal(validateWebSocketRequest(invalidSession)?.status, 400);
});
