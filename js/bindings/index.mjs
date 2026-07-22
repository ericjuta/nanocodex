const AGENT_STATE = Symbol("nanocodex.agent");
const TURN_STATE = Symbol("nanocodex.turn");

let nextUid = 1;

export const Engine = Object.freeze({
  from(definition) {
    if (!definition || typeof definition.create !== "function") {
      throw new TypeError("a Nanocodex engine must define create(config)");
    }
    return Object.freeze({
      key: definition.key || "custom",
      name: definition.name || definition.key || "Custom engine",
      type: definition.type || "custom",
      create: definition.create,
      dispose: definition.dispose || ((agent) => agent.free()),
      subscribe: definition.subscribe,
    });
  },
});

export const Agent = Object.freeze({
  async create(options) {
    const { engine, ...config } = options || {};
    if (!engine || typeof engine.create !== "function") {
      throw new TypeError("Agent.create requires an engine");
    }
    const raw = await engine.create(config);
    return createAgent(raw, engine).extend(agentActions());
  },

  from(raw, options) {
    if (!options?.engine) throw new TypeError("Agent.from requires an engine");
    return createAgent(raw, options.engine).extend(agentActions());
  },
});

export const Turn = Object.freeze({
  from(raw, options = {}) {
    return createTurn(raw, options.agent);
  },
});

function prompt(agent, options) {
  const state = agentState(agent);
  const input = actionInput(options);
  const raw = typeof input === "string"
    ? state.raw.prompt(input)
    : state.raw.promptContent(JSON.stringify(input));
  return createTurn(raw, agent);
}

function result(turn) {
  const state = turnState(turn);
  state.result ||= Promise.resolve().then(() => state.raw.result());
  return state.result;
}

function steer(turn, options) {
  const state = turnState(turn);
  const input = actionInput(options);
  return typeof input === "string"
    ? state.raw.steer(input)
    : state.raw.steerContent(JSON.stringify(input));
}

function cancel(turn) {
  return turnState(turn).raw.cancel();
}

async function forkLatest(agent) {
  const state = agentState(agent);
  return createAgent(await state.raw.fork(), state.engine).extend(agentActions());
}

async function forkFrom(agent, options) {
  const state = agentState(agent);
  const checkpoint = options?.turn;
  return createAgent(
    await state.raw.forkFrom(turnState(checkpoint).raw),
    state.engine,
  ).extend(agentActions());
}

async function spawn(agent) {
  const state = agentState(agent);
  return createAgent(await state.raw.spawn(), state.engine).extend(agentActions());
}

function watchEvents(agent, options) {
  const state = agentState(agent);
  if (typeof state.engine.subscribe !== "function") {
    throw new Error(`${state.engine.name} does not expose agent events`);
  }
  const onEvent = typeof options === "function" ? options : options?.onEvent;
  if (typeof onEvent !== "function") {
    throw new TypeError("events.watch requires an onEvent callback");
  }
  const includeAllSessions = typeof options === "object" && options?.includeAllSessions === true;
  return state.engine.subscribe((event) => {
    if (includeAllSessions || !event?.request_id || event.request_id === agent.sessionId) {
      onEvent(event);
    }
  });
}

export const Actions = Object.freeze({
  turn: Object.freeze({ prompt, result, steer, cancel }),
  fork: Object.freeze({ latest: forkLatest, from: forkFrom }),
  agent: Object.freeze({ spawn }),
  events: Object.freeze({ watch: watchEvents }),
  getAction(client, action, path) {
    const attached = path.split(".").reduce((value, key) => value?.[key], client);
    return typeof attached === "function" ? attached : (...args) => action(client, ...args);
  },
});

export function agentActions() {
  return (agent) => ({
    turn: {
      prompt: (options) => prompt(agent, options),
    },
    fork: {
      latest: () => forkLatest(agent),
      from: (options) => forkFrom(agent, options),
    },
    events: {
      watch: (options) => watchEvents(agent, options),
    },
    spawn: () => spawn(agent),
  });
}

export function toWasmConfig(options = {}) {
  const apiKey = options.apiKey;
  if (typeof apiKey !== "string" || !apiKey.trim()) {
    throw new TypeError("apiKey must be a non-empty string");
  }
  const config = { api_key: apiKey };
  copy(config, "thinking", options.thinking);
  copy(config, "reasoning_mode", options.reasoningMode);
  copy(config, "websocket_url", options.websocketUrl);
  copy(config, "api_base_url", options.apiBaseUrl);
  copy(config, "instructions", options.instructions);
  copy(config, "session_id", options.sessionId);
  return config;
}

export function createEventChannel(onEvent) {
  const listeners = new Set();
  return Object.freeze({
    emit(eventJson) {
      const event = typeof eventJson === "string" ? JSON.parse(eventJson) : eventJson;
      onEvent?.(event);
      for (const listener of listeners) listener(event);
    },
    subscribe(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
  });
}

function createAgent(raw, engine, extensions = {}) {
  if (!raw || typeof raw.prompt !== "function") {
    throw new TypeError("Agent.from requires a Nanocodex WASM handle");
  }
  const state = { raw, engine, disposed: false };
  return agentView(state, extensions);
}

function agentView(state, extensions) {
  const client = {
    uid: `agent-${nextUid++}`,
    get sessionId() {
      return state.raw.sessionId;
    },
    engine: state.engine,
    extend(extension) {
      const value = typeof extension === "function" ? extension(client) : extension;
      return agentView(state, deepMerge(extensions, value || {}));
    },
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      state.engine.dispose(state.raw);
    },
  };
  Object.defineProperty(client, AGENT_STATE, { value: state });
  return Object.assign(client, extensions);
}

function createTurn(raw, agent) {
  if (!raw || typeof raw.result !== "function") {
    throw new TypeError("Turn.from requires a Nanocodex WASM turn handle");
  }
  const state = { raw, agent, result: undefined, disposed: false };
  const turn = {
    get agent() {
      return state.agent;
    },
    result: () => result(turn),
    steer: (options) => steer(turn, options),
    cancel: () => cancel(turn),
    dispose() {
      if (state.disposed) return;
      state.disposed = true;
      state.raw.free();
    },
  };
  Object.defineProperty(turn, TURN_STATE, { value: state });
  return Object.freeze(turn);
}

function agentState(agent) {
  const state = agent?.[AGENT_STATE];
  if (!state) throw new TypeError("expected a Nanocodex agent client");
  if (state.disposed) throw new Error("the Nanocodex agent has been disposed");
  return state;
}

function turnState(turn) {
  const state = turn?.[TURN_STATE];
  if (!state) throw new TypeError("expected a Nanocodex turn");
  if (state.disposed) throw new Error("the Nanocodex turn has been disposed");
  return state;
}

function actionInput(options) {
  const input = typeof options === "string" || Array.isArray(options) ? options : options?.input;
  if (typeof input !== "string" && !Array.isArray(input)) {
    throw new TypeError("turn input must be a string or ordered content array");
  }
  return input;
}

function deepMerge(left, right) {
  const merged = { ...left };
  for (const [key, value] of Object.entries(right)) {
    merged[key] = isObject(merged[key]) && isObject(value)
      ? deepMerge(merged[key], value)
      : value;
  }
  return merged;
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function copy(target, key, value) {
  if (value !== undefined) target[key] = value;
}
