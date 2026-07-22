# nanocodex-react

React state management for a browser-embedded Nanocodex agent. The package is
presentation-free and does not choose a Worker script, credential endpoint, or
application health check.

```tsx
const config = createNanocodexConfig({
  reasoningMode: "pro",
  thinking: "high",
  createWorker: () => new Worker(new URL("./agent.worker.ts", import.meta.url), { type: "module" }),
});

root.render(
  <NanocodexProvider config={config}>
    <App />
  </NanocodexProvider>,
);
```

Use `useNanocodexState()` for readiness, `useNanocodexMessages()` for the
ordered agent stream, or `useNanocodex()` for the combined compatibility
surface.
