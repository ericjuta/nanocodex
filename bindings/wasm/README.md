# Node.js and browser WebAssembly binding

`nanocodex-wasm` compiles the shared Rust model loop, typed history, prompt
cache behavior, event protocol, Responses request/stream handling, and Tower
retry stack to `wasm32-unknown-unknown`. JavaScript supplies only the host
capabilities Rust cannot own on that target: a WebSocket and code execution.

Build both packages and run their deterministic tests:

```sh
just bootstrap-bindings
just test-wasm
```

The Node example uses the generated CommonJS binding and defines an application
tool as an ordinary async JavaScript function:

```sh
OPENAI_API_KEY=... node bindings/wasm/node/example.mjs
```

See [`node/example.mjs`](node/example.mjs) for persistent follow-on prompting,
events, and a custom `multiply` tool. `prompt()` returns a `Turn` synchronously;
`await turn.result()` returns the final message. The Rust-owned session retains
all prior messages, response IDs, tool outputs, WebSocket state, and the stable
prompt-cache key.

[`browser/worker.mjs`](browser/worker.mjs) shows the same API inside a module
Worker. Standard browser WebSockets cannot attach the `Authorization` header
required by the Responses upgrade, so the web host deliberately accepts a
caller-provided `createWebSocket` or an already-authorized WebSocket endpoint.
Nanocodex does not add an app server or credential relay. A product embedding
chooses its own credential and endpoint boundary while the agent stays inside
the Worker.

JavaScript tools are described with a name, description, JSON Schema, and
handler. They are injected into code mode as `tools.<name>(input)`, and their
calls/results are folded into the same ordered Rust `AgentEvent` stream as the
parent `exec` call.
