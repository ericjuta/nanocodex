# JavaScript libraries

- [`bindings`](bindings) publishes `nanocodex`: the runtime-neutral agent and
  action API plus the Node and browser WASM engines.
- [`react`](react) publishes `nanocodex-react`: the external store, provider,
  and hooks for a browser Worker owned by the embedding application.

Applications consume package entrypoints. Generated `wasm-bindgen` output
stays private to `nanocodex` and is produced by `just build-wasm`.
