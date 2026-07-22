export type NanocodexThinking = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type NanocodexReasoningMode = "standard" | "pro";
export type NanocodexWorkerMessage = { type: string; [key: string]: unknown };
export type NanocodexWorkerCommand = { type: string; [key: string]: unknown };

export type NanocodexState = Readonly<{
  ready: boolean;
  configured: boolean | null;
  credentialSource: "user" | "deployment" | null;
  stopped: boolean;
}>;

export type NanocodexWorker = {
  onmessage: ((event: MessageEvent<NanocodexWorkerMessage>) => void) | null;
  postMessage(message: NanocodexWorkerCommand): void;
  terminate(): void;
};

export type NanocodexConfig = Readonly<{
  getState(): NanocodexState;
  subscribe(listener: () => void): () => void;
  subscribeMessages(listener: (message: NanocodexWorkerMessage) => void): () => void;
  mount(): () => void;
  send(command: NanocodexWorkerCommand): void;
  stop(): void;
}>;

export type CreateNanocodexConfigOptions = {
  thinking?: NanocodexThinking;
  reasoningMode?: NanocodexReasoningMode;
  createWorker(): NanocodexWorker;
  checkHealth?(): Promise<{
    agent_configured?: boolean;
    credential_source?: "user" | "deployment" | null;
  }>;
};

export function createNanocodexConfig(options: CreateNanocodexConfigOptions): NanocodexConfig;
