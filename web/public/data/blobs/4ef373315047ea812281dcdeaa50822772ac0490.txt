import {
  DEFAULT_RESPONSES_UPGRADE_URL,
  upstreamHeaders,
  validateWebSocketRequest,
} from "./protocol.mjs";

export default {
  async fetch(request, env) {
    const rejection = validateWebSocketRequest(request);
    if (rejection) return rejection;
    if (!env.OPENAI_API_KEY) return new Response("Worker secret is not configured", { status: 500 });

    const upstreamResponse = await fetch(DEFAULT_RESPONSES_UPGRADE_URL, {
      headers: upstreamHeaders(env.OPENAI_API_KEY, new URL(request.url).searchParams.get("session_id")),
    });
    const upstream = upstreamResponse.webSocket;
    if (!upstream) {
      upstreamResponse.body?.cancel();
      return new Response("OpenAI WebSocket upgrade failed", { status: 502 });
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    upstream.accept();
    server.accept();
    bridge(server, upstream);

    return new Response(null, { status: 101, webSocket: client });
  },
};

function bridge(browser, upstream) {
  forward(browser, upstream);
  forward(upstream, browser);
}

function forward(source, destination) {
  source.addEventListener("message", (event) => {
    if (typeof event.data !== "string") {
      close(destination, 1003, "text frames required");
      close(source, 1003, "text frames required");
      return;
    }
    if (destination.readyState === 1) destination.send(event.data);
  });
  source.addEventListener("close", (event) => close(destination, event.code, event.reason || "peer closed"));
  source.addEventListener("error", () => close(destination, 1011, "peer WebSocket failed"));
}

function close(socket, code, reason) {
  if (socket.readyState !== 0 && socket.readyState !== 1) return;
  const safeCode = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1011;
  socket.close(safeCode, reason.slice(0, 120));
}
