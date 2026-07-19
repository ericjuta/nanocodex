import init, { Nanocodex } from "../../../bindings/wasm/pkg-web/nanocodex.js";
import { createBrowserHost } from "../../../bindings/wasm/browser/host.mjs";

type StartMessage = {
  type: "start";
  websocketUrl: string;
  thinking: "low" | "medium" | "high" | "xhigh";
};

type PromptMessage = {
  type: "prompt";
  id: number;
  prompt: string;
};

type IncomingMessage = StartMessage | PromptMessage;

const worker = self as DedicatedWorkerGlobalScope;

const hostGlobal = globalThis as typeof globalThis & { nanocodexHost: unknown };
hostGlobal.nanocodexHost = createBrowserHost({
  // Browser WebSockets cannot attach an Authorization header. The URL must be
  // authorized by the embedding application, for example through a short-lived
  // signed URL or same-site session cookie.
  createWebSocket: (endpoint: string) => new WebSocket(endpoint),
  onEvent: (eventJson: string) => {
    worker.postMessage({ type: "event", event: JSON.parse(eventJson) });
  },
  tools: {
    browserInfo: {
      description: "Return basic information about the browser Worker runtime.",
      parameters: { type: "object", additionalProperties: false },
      handler: async () => ({
        language: navigator.language,
        online: navigator.onLine,
        userAgent: navigator.userAgent,
      }),
    },
  },
});

await init();

let agent: Nanocodex | undefined;

worker.onmessage = ({ data }: MessageEvent<IncomingMessage>) => {
  if (data.type === "start") {
    agent?.free();
    agent = new Nanocodex(JSON.stringify({
      api_key: "host-managed",
      websocket_url: data.websocketUrl,
      thinking: data.thinking,
      workspace: "/browser",
    }));
    worker.postMessage({ type: "ready" });
    return;
  }

  const current = agent;
  if (!current) {
    worker.postMessage({ type: "error", id: data.id, message: "Start the agent first." });
    return;
  }

  // Each prompt gets an independent Turn, while the owned agent serializes
  // them onto the same session and preserves all follow-on context.
  const turn = current.prompt(data.prompt);
  void turn.result().then(
    (message) => worker.postMessage({ type: "result", id: data.id, message }),
    (error) => worker.postMessage({
      type: "error",
      id: data.id,
      message: error instanceof Error ? error.message : String(error),
    }),
  );
};
