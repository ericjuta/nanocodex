# React + Vite Worker example

This app embeds the browser build of `nanocodex-wasm` in a module Worker. The
Worker owns one persistent `Nanocodex` session, forwards its ordered events to
React, and registers the browser-native `browserInfo` tool.

```sh
just bootstrap-bindings
just build-react-example
just dev-react-example
```

`just dev-react-example` loads `OPENAI_API_KEY` from the repository environment
and runs both sides through Cloudflare's Vite plugin: React in Vite and the API
Worker in the local `workerd` runtime. Start the agent and submit any number of
follow-on prompts. The Rust-owned session keeps its response chain and complete
history; React never passes prior results back to it.

The Cloudflare Worker handles `/api/responses`, reads `OPENAI_API_KEY` from its
secret binding, and opens the default `wss://api.openai.com/v1/responses`
connection. The key is never sent to React or the browser Worker. For deployment:

```sh
cd examples/react-vite
npx wrangler secret put OPENAI_API_KEY
npm run deploy
```
