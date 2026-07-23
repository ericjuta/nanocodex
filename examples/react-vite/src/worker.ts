import { Agent, type ReasoningMode, type Thinking } from "nanocodex/browser";

type StartMessage = {
  type: "start";
  thinking: Thinking;
  reasoningMode?: ReasoningMode;
};

type PromptMessage = {
  type: "prompt";
  id: number;
  prompt: string;
};

type IncomingMessage = StartMessage | PromptMessage;

const worker = self as DedicatedWorkerGlobalScope;

let agent: Agent.Agent | undefined;
let eventWatch: ReturnType<Agent.Agent["events"]["watch"]> | undefined;

worker.onmessage = ({ data }: MessageEvent<IncomingMessage>) => {
  void handleMessage(data);
};

async function handleMessage(data: IncomingMessage): Promise<void> {
  if (data.type === "start") {
    eventWatch?.off();
    eventWatch = undefined;
    agent?.dispose();
    agent = await Agent.create({
      apiKey: "worker-managed",
      websocketUrl: workerEndpoint(),
      // Browser WebSockets cannot attach an Authorization header. The URL must
      // be authorized by the embedding application.
      createWebSocket: (endpoint: string, sessionId: string) => {
        const url = new URL(endpoint);
        url.searchParams.set("session_id", sessionId);
        return new WebSocket(url);
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
      thinking: data.thinking,
      reasoningMode: data.reasoningMode,
    });
    eventWatch = agent.events.watch();
    eventWatch.onEvent((event) => worker.postMessage({ type: "event", event }));
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
  const turn = current.turn.prompt({ input: data.prompt });
  void turn.result().then(
    (message) => worker.postMessage({ type: "result", id: data.id, message }),
    (error) => worker.postMessage({
      type: "error",
      id: data.id,
      message: error instanceof Error ? error.message : String(error),
    }),
  );
}

function workerEndpoint(): string {
  const protocol = self.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${self.location.host}/api/responses`;
}
