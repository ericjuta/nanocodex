# Vendored cljrs patches

Nanocodex vendors `cljrs-env` and `cljrs-async` version 0.1.228 from
[csm/clojurust](https://github.com/csm/clojurust), upstream commit
`513ecd4ab950128cc8e08e76c0af65e696518dc5`.

The remaining cljrs crates are resolved from crates.io at the exact same
version. These two crates are patched narrowly for Nanocodex Code Mode:

- `cljrs-env` adds an isolate-local restricted execution policy, namespace
  visibility checks, and externally triggered cooperative cancellation.
- `cljrs-async` adds fallible isolate startup and cell-scoped ownership,
  cancellation, and joining of locally spawned Clojure futures.

Keep these patches rebased as one unit when upgrading cljrs. The upstream code
and local modifications are licensed under EPL-1.0; see `COPYING.md`.
