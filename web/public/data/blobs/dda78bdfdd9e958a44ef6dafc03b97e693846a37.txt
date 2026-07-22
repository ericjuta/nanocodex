export const DEFAULT_RESPONSES_UPGRADE_URL = "https://api.openai.com/v1/responses";

const RESPONSES_WEBSOCKETS_BETA = "responses_websockets=2026-02-06";

export function validateWebSocketRequest(request) {
  const url = new URL(request.url);
  if (url.pathname !== "/api/responses") return new Response("Not found", { status: 404 });
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("Expected WebSocket upgrade", { status: 426 });
  }
  if (!sameOrigin(request, url)) return new Response("Forbidden", { status: 403 });
  if (!validSessionId(url.searchParams.get("session_id"))) {
    return new Response("Invalid session", { status: 400 });
  }
  return undefined;
}

export function upstreamHeaders(apiKey, sessionId) {
  return {
    Upgrade: "websocket",
    Authorization: `Bearer ${apiKey}`,
    "OpenAI-Beta": RESPONSES_WEBSOCKETS_BETA,
    "x-openai-internal-codex-responses-lite": "true",
    "session-id": sessionId,
    "thread-id": sessionId,
    "x-client-request-id": sessionId,
    "x-responsesapi-include-timing-metrics": "true",
    "User-Agent": "nanocodex-react-vite/0.1.0",
  };
}

function sameOrigin(request, url) {
  const origin = request.headers.get("Origin");
  if (!origin) return false;
  try {
    return new URL(origin).host === url.host;
  } catch {
    return false;
  }
}

function validSessionId(sessionId) {
  return typeof sessionId === "string" && /^[A-Za-z0-9._:-]{1,200}$/.test(sessionId);
}
