import type { Agent, PromptInput, Turn } from "../types.mjs";

/** Accepts a prompt on an owned Agent and returns its independently awaitable Turn. */
export function prompt<const agent extends Agent<object>>(
  agent: agent,
  options: prompt.Options,
): prompt.ReturnType<agent>;
export declare namespace prompt {
  type Options = { input: PromptInput };
  type ReturnType<agent extends Agent<object> = Agent<object>> = Turn<agent>;
}

/** Waits for a Turn's final assistant message. */
export function getResult(turn: Turn): Promise<getResult.ReturnType>;
export declare namespace getResult {
  type ReturnType = string;
}

/** Adds input to an active Turn. */
export function steer(turn: Turn, options: steer.Options): Promise<void>;
export declare namespace steer {
  type Options = { input: PromptInput };
  type ReturnType = void;
}

/** Cancels an active or queued Turn. */
export function cancel(turn: Turn): Promise<void>;
export declare namespace cancel {
  type ReturnType = void;
}
