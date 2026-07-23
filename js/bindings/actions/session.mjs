import { fork as forkAgent, spawn as spawnAgent } from "../internal.mjs";

export function fork(agent, options = {}) {
  return forkAgent(agent, options);
}

export function spawn(agent) {
  return spawnAgent(agent);
}
