export function createNanocodexConfig(options) {
  if (typeof options?.createWorker !== "function") {
    throw new TypeError("createNanocodexConfig requires createWorker");
  }
  const thinking = options.thinking ?? "medium";
  const reasoningMode = options.reasoningMode ?? "standard";
  const stateListeners = new Set();
  const messageListeners = new Set();
  let state = Object.freeze({ ready: false, configured: null, credentialSource: null, stopped: false });
  let worker;
  let mounts = 0;
  let healthRequest = 0;

  function setState(patch) {
    const next = Object.freeze({ ...state, ...patch });
    if (
      next.ready === state.ready
      && next.configured === state.configured
      && next.credentialSource === state.credentialSource
      && next.stopped === state.stopped
    ) return;
    state = next;
    for (const listener of stateListeners) listener();
  }

  function connect() {
    if (worker || state.stopped) return;
    const current = options.createWorker();
    worker = current;
    current.onmessage = ({ data }) => {
      if (data.type === "ready") setState({ ready: true });
      for (const listener of messageListeners) listener(data);
    };
    current.postMessage({ type: "start", thinking, reasoningMode });
    if (!options.checkHealth) return;
    const request = ++healthRequest;
    void options.checkHealth().then(
      (health) => {
        if (request === healthRequest) {
          setState({
            configured: health.agent_configured === true,
            credentialSource: health.credential_source === "user" || health.credential_source === "deployment"
              ? health.credential_source
              : null,
          });
        }
      },
      () => {
        if (request === healthRequest) setState({ configured: null, credentialSource: null });
      },
    );
  }

  function disconnect() {
    healthRequest += 1;
    worker?.terminate();
    worker = undefined;
    setState({ ready: false });
  }

  return Object.freeze({
    getState: () => state,
    subscribe(listener) {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    subscribeMessages(listener) {
      messageListeners.add(listener);
      return () => messageListeners.delete(listener);
    },
    mount() {
      mounts += 1;
      connect();
      let mounted = true;
      return () => {
        if (!mounted) return;
        mounted = false;
        mounts -= 1;
        if (mounts === 0) disconnect();
      };
    },
    send(command) {
      worker?.postMessage(command);
    },
    stop() {
      disconnect();
      setState({ stopped: true });
    },
  });
}
