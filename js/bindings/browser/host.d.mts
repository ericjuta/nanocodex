export type BrowserTool = {
  description: string;
  parameters: Record<string, unknown>;
  handler: (
    input: unknown,
    context: { sessionId: string },
  ) => unknown | Promise<unknown>;
};

export type BrowserToolMap = Record<string, BrowserTool>;

export function createBrowserHost(options?: {
  WebSocketImpl?: typeof WebSocket;
  createWebSocket?: (endpoint: string, sessionId: string) => WebSocket;
  onEvent?: (eventJson: string) => void;
  tools?: BrowserToolMap;
  maxQueuedMessages?: number;
  maxQueuedBytes?: number;
  maxBufferedSendBytes?: number;
}): unknown;
