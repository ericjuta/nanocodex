# nanocodex-react

Small React bindings for an application-owned Nanocodex Worker. The package
owns worker lifecycle and an external store; credentials, health checks,
transport policy, and presentation stay in the application.

```tsx
import { createConfig, NanocodexProvider } from "nanocodex-react";

const config = createConfig({
  worker: () => new Worker(new URL("./agent.worker.ts", import.meta.url), { type: "module" }),
  reasoningMode: "pro",
  thinking: "high",
});

root.render(
  <NanocodexProvider config={config}>
    <App />
  </NanocodexProvider>,
);
```

`useNanocodex()` returns the worker status plus `dispatch` and `stop`.
`useNanocodexMessage()` subscribes to ordered worker messages, and `useConfig()`
exposes the headless store for advanced composition. Command and message unions
can be supplied as generics to keep the entire Worker boundary typed.
