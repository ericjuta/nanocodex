# Node.js PoC

This example consumes the publishable `nanocodex` package exactly like an
external Node application. The Node host supplies the WebSocket, API key, and
an ordinary JavaScript `multiply` tool; the Rust/WASM engine owns the agent
lifecycle, tool loop, retained conversation, and follow-on response chain.

From this directory:

```sh
npm install
OPENAI_API_KEY=... npm start
```

`npm start` also reads the repository's ignored `.env` file when present. The
key remains in the Node process and is used by the Node WebSocket host; it is
not compiled into the WASM artifact or the npm package.
