import type {
  AgentOptions,
  DefaultAgent,
  ToolMap,
} from "../types.mjs";

export type Agent = DefaultAgent;

/** Creates a Node-hosted Rust/WASM Agent. */
export function create(options: create.Options): Promise<create.ReturnType>;
export declare namespace create {
  type Options = AgentOptions & {
    apiBaseUrl?: string | undefined;
    apiKey: string;
    connectTimeoutMs?: number | undefined;
    maxFrameBytes?: number | undefined;
    maxQueuedBytes?: number | undefined;
    maxQueuedMessages?: number | undefined;
    sendTimeoutMs?: number | undefined;
    tools?: ToolMap | undefined;
    websocketUrl?: string | undefined;
  };
  type ReturnType = Agent;
}
