# Nanocodex examples

All language consumers live at this repository boundary:

- Rust: `minimal.rs`, `follow_on.rs`, `custom_tool.rs`, `subagents.rs`, and
  `mcp.rs` are binaries in the `nanocodex-examples` package.
- Python: `python/` uses the native PyO3 binding.
- Node.js: `node/` uses the shared Rust/WASM package with a Node WebSocket host.
- Browser: `react-vite/` runs that WASM agent in a module Worker and renders its
  ordered events in React.

From the repository root:

```sh
cargo run -p nanocodex-examples --bin minimal
cargo run -p nanocodex-examples --bin mcp
just smoke-python
just smoke-wasm-node
just build-react-example
```

The live programs require `OPENAI_API_KEY`. The browser example instead asks
the embedding application for an already-authorized Responses WebSocket URL;
standard browser WebSockets cannot attach the upgrade authorization header.

The MCP example defaults to the public OpenAI documentation MCP. Override
`NANOCODEX_MCP_URL` for another Streamable HTTP server and set
`NANOCODEX_MCP_BEARER_TOKEN` when it requires bearer authentication.
