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

The public JavaScript layer is shared by Node and the browser. It keeps the
generated WASM handles private and exposes a small, namespaced API:

```js
import { Actions } from "nanocodex";
import { Agent } from "nanocodex/node";

const agent = await Agent.create({
  apiKey: process.env.OPENAI_API_KEY,
  tools,
  reasoningMode: "pro",
  thinking: "high", // none, low, medium, high, xhigh, or max
});

const turn = agent.turn.prompt({ input: "Build the thing." });
console.log(await turn.result());

// Every decorated operation is also available as a standalone action.
const followOn = Actions.turn.prompt(agent, { input: "Now explain it." });
console.log(await Actions.turn.result(followOn));
```

The runtime entry point owns its obvious defaults: `nanocodex/node` flattens
Node host and agent policy into one `Agent.create(...)` call. The package root
retains the lower-level environment-neutral `Agent` and `Engine`, while
`node()` and `browser()` remain available for reusable or custom engine
composition. Agent actions are grouped under `turn`, `fork`, and `events`;
`agent.extend(...)` adds application actions without replacing the owned
session lifecycle.

The Node example is a standalone npm consumer and defines an application tool
as an ordinary async JavaScript function:

```sh
cd examples/node
npm install
OPENAI_API_KEY=... npm start
```

See [`examples/node`](../../examples/node) for persistent follow-on prompting,
events, and a custom `multiply` tool. `agent.turn.prompt()` returns a `Turn`
synchronously; `await turn.result()` returns the final message. A turn can be
steered or cancelled, and an agent can fork its latest checkpoint, fork an
exact completed turn, or spawn a clean sibling. The Rust-owned session
retains all prior messages, response IDs, tool outputs, WebSocket state, and the
stable prompt-cache key. Browser-safe typed content accepts ordered text,
remote/data-URL image, and audio items while rejecting local filesystem paths.

[`examples/react-vite`](../../examples/react-vite) is a complete React + Vite
consumer using the same API inside a module Worker. Standard browser WebSockets
cannot attach the `Authorization` header required by the Responses upgrade, so
the Worker accepts an already-authorized WebSocket endpoint. Nanocodex does not
add an app server or credential relay. A product embedding chooses its own
credential and endpoint boundary while the agent stays inside the Worker.

JavaScript tools are described with a name, description, JSON Schema, and
handler. They are injected into code mode as `tools.<name>(input)`, and their
calls/results are folded into the same ordered Rust `AgentEvent` stream as the
parent `exec` call.
