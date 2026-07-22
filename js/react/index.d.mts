import type { ReactNode } from "react";

import type {
  NanocodexConfig,
  NanocodexState,
  NanocodexWorkerCommand,
  NanocodexWorkerMessage,
} from "./config.mjs";

export {
  createNanocodexConfig,
  type CreateNanocodexConfigOptions,
  type NanocodexConfig,
  type NanocodexState,
  type NanocodexThinking,
  type NanocodexWorker,
  type NanocodexWorkerCommand,
  type NanocodexWorkerMessage,
} from "./config.mjs";

export type NanocodexReact = NanocodexState & {
  send(command: NanocodexWorkerCommand): void;
  subscribe(listener: (message: NanocodexWorkerMessage) => void): () => void;
  stop(): void;
};

export function NanocodexProvider(props: {
  children: ReactNode;
  config: NanocodexConfig;
}): ReactNode;

export function useNanocodexConfig(): NanocodexConfig;
export function useNanocodexState(): NanocodexState;
export function useNanocodex(): NanocodexReact;
export function useNanocodexMessages(
  listener: (message: NanocodexWorkerMessage) => void,
): void;
