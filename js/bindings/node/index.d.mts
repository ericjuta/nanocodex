import type { Agent as CoreAgent, AgentEvent, AgentOptions, Engine } from "../index.mjs";

export type Tool = {
  description: string;
  parameters: Record<string, unknown>;
  handler(input: unknown): unknown | Promise<unknown>;
};

export type NodeEngineOptions = {
  apiKey: string;
  websocketUrl?: string;
  apiBaseUrl?: string;
  tools?: Record<string, Tool>;
  onEvent?(event: AgentEvent): void;
  connectTimeoutMs?: number;
  sendTimeoutMs?: number;
  maxQueuedMessages?: number;
  maxQueuedBytes?: number;
  maxFrameBytes?: number;
};

export type NodeAgentOptions = Omit<AgentOptions, "apiKey" | "websocketUrl" | "apiBaseUrl"> & NodeEngineOptions;

export const Agent: Readonly<{
  create(options: NodeAgentOptions): Promise<CoreAgent.Client>;
}>;

/** Low-level reusable engine factory for custom composition. */
export function node(options: NodeEngineOptions): Engine.Instance;
