import {
  createContext,
  createElement,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";

export { createNanocodexConfig } from "./config.mjs";

const NanocodexContext = createContext(null);

export function NanocodexProvider({ children, config }) {
  if (!config) throw new TypeError("NanocodexProvider requires a config");
  useEffect(() => config.mount(), [config]);
  return createElement(NanocodexContext.Provider, { value: config }, children);
}

export function useNanocodexConfig() {
  const config = useContext(NanocodexContext);
  if (!config) throw new Error("Nanocodex hooks must be used inside NanocodexProvider");
  return config;
}

export function useNanocodexState() {
  const config = useNanocodexConfig();
  return useSyncExternalStore(config.subscribe, config.getState, config.getState);
}

export function useNanocodex() {
  const config = useNanocodexConfig();
  const state = useSyncExternalStore(config.subscribe, config.getState, config.getState);
  return useMemo(() => ({
    ...state,
    send: config.send,
    subscribe: config.subscribeMessages,
    stop: config.stop,
  }), [config, state]);
}

export function useNanocodexMessages(listener) {
  const config = useNanocodexConfig();
  const latest = useRef(listener);
  latest.current = listener;
  useEffect(
    () => config.subscribeMessages((message) => latest.current(message)),
    [config],
  );
}
