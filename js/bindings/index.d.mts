export type Thinking = "none" | "low" | "medium" | "high" | "xhigh" | "max";
export type ReasoningMode = "standard" | "pro";

export type PromptItem =
  | { type: "text"; text: string }
  | { type: "image"; image_url: string; detail?: "auto" | "low" | "high" | "original" }
  | { type: "audio"; audio_url: string };

export type PromptInput = string | readonly PromptItem[];

export type AgentEvent = {
  type: string;
  request_id?: string;
  sequence?: number;
  payload?: unknown;
  [key: string]: unknown;
};

export type AgentOptions = {
  apiKey?: string;
  thinking?: Thinking;
  reasoningMode?: ReasoningMode;
  websocketUrl?: string;
  apiBaseUrl?: string;
  instructions?: string;
  sessionId?: string;
};

export type RawTurn = {
  result(): Promise<string>;
  steer(instruction: string): Promise<void>;
  steerContent(contentJson: string): Promise<void>;
  cancel(): Promise<void>;
  free(): void;
};

export type RawAgent = {
  readonly sessionId: string;
  prompt(instruction: string): RawTurn;
  promptContent(contentJson: string): RawTurn;
  fork(): Promise<RawAgent>;
  forkFrom(turn: RawTurn): Promise<RawAgent>;
  spawn(): Promise<RawAgent>;
  free(): void;
};

export namespace Engine {
  type Definition = {
    key?: string;
    name?: string;
    type?: string;
    create(options: AgentOptions): RawAgent | Promise<RawAgent>;
    dispose?(agent: RawAgent): void;
    subscribe?(listener: (event: AgentEvent) => void): () => void;
  };

  type Instance = Readonly<Required<Pick<Definition, "key" | "name" | "type" | "create" | "dispose">> & Pick<Definition, "subscribe">>;

  function from(definition: Definition): Instance;
}

export namespace Turn {
  type Client = {
    readonly agent?: Agent.Client;
    result(): Promise<string>;
    steer(options: Actions.turn.steer.Options | PromptInput): Promise<void>;
    cancel(): Promise<void>;
    dispose(): void;
  };

  function from(raw: RawTurn, options?: { agent?: Agent.Client }): Client;
}

export namespace Agent {
  type CreateOptions = AgentOptions & { engine: Engine.Instance };
  type Extension<T extends object> = T | ((agent: Client) => T);
  type Client<T extends object = AgentActions> = {
    readonly uid: string;
    readonly sessionId: string;
    readonly engine: Engine.Instance;
    extend<U extends object>(extension: Extension<U>): Client<T & U>;
    dispose(): void;
  } & T;

  function create(options: CreateOptions): Promise<Client>;
  function from(raw: RawAgent, options: { engine: Engine.Instance }): Client;
}

export type AgentActions = {
  turn: {
    prompt(options: Actions.turn.prompt.Options | PromptInput): Turn.Client;
  };
  fork: {
    latest(): Promise<Agent.Client>;
    from(options: Actions.fork.from.Options): Promise<Agent.Client>;
  };
  events: {
    watch(options: Actions.events.watch.Options | ((event: AgentEvent) => void)): () => void;
  };
  spawn(): Promise<Agent.Client>;
};

export namespace Actions {
  namespace turn {
    namespace prompt { type Options = { input: PromptInput }; }
    function prompt(agent: Agent.Client, options: prompt.Options | PromptInput): Turn.Client;

    namespace result { type ReturnType = Promise<string>; }
    function result(turn: Turn.Client): result.ReturnType;

    namespace steer { type Options = { input: PromptInput }; }
    function steer(turn: Turn.Client, options: steer.Options | PromptInput): Promise<void>;
    function cancel(turn: Turn.Client): Promise<void>;
  }

  namespace fork {
    function latest(agent: Agent.Client): Promise<Agent.Client>;
    namespace from { type Options = { turn: Turn.Client }; }
    function from(agent: Agent.Client, options: from.Options): Promise<Agent.Client>;
  }

  namespace agent {
    function spawn(agent: Agent.Client): Promise<Agent.Client>;
  }

  namespace events {
    namespace watch {
      type Options = { onEvent(event: AgentEvent): void; includeAllSessions?: boolean };
    }
    function watch(agent: Agent.Client, options: watch.Options | ((event: AgentEvent) => void)): () => void;
  }

  function getAction<T extends (...args: never[]) => unknown>(
    client: Agent.Client,
    action: (client: Agent.Client, ...args: Parameters<T>) => ReturnType<T>,
    path: string,
  ): T;
}

export function agentActions(): (agent: Agent.Client) => AgentActions;

export function toWasmConfig(options?: AgentOptions): {
  api_key: string;
  thinking?: Thinking;
  reasoning_mode?: ReasoningMode;
  websocket_url?: string;
  api_base_url?: string;
  instructions?: string;
  session_id?: string;
};

export function createEventChannel(onEvent?: (event: AgentEvent) => void): Readonly<{
  emit(event: string | AgentEvent): void;
  subscribe(listener: (event: AgentEvent) => void): () => void;
}>;
