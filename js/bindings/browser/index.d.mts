import type { AgentEvent, Engine } from "../index.mjs";
import type { BrowserTool, BrowserToolMap } from "./host.mjs";

export type BrowserEngineOptions = {
  apiKey?: string;
  websocketUrl?: string;
  apiBaseUrl?: string;
  module?: unknown;
  tools?: BrowserToolMap;
  onEvent?(event: AgentEvent): void;
  WebSocketImpl?: typeof WebSocket;
  createWebSocket?(endpoint: string, sessionId: string): WebSocket;
  maxQueuedMessages?: number;
  maxQueuedBytes?: number;
  maxBufferedSendBytes?: number;
};

export type { BrowserTool, BrowserToolMap };

export function browser(options?: BrowserEngineOptions): Engine.Instance;
