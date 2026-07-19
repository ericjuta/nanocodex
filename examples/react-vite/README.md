# React + Vite Worker example

This app embeds the browser build of `nanocodex-wasm` in a module Worker. The
Worker owns one persistent `Nanocodex` session, forwards its ordered events to
React, and registers the browser-native `browserInfo` tool.

```sh
just bootstrap-bindings
just build-react-example
npm run dev --prefix examples/react-vite
```

Paste an already-authorized Responses WebSocket URL into the connection form,
start the agent, and submit any number of follow-on prompts. The Rust-owned
session keeps its response chain and complete history; React never passes prior
results back to it.

The URL is deliberately application-owned. A browser WebSocket cannot set the
Responses `Authorization` upgrade header, and this library does not introduce a
credential relay or app server.
