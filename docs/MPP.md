# MPP Responses WebSocket integration

## Boundary

MPP is composed only by `bin/nanocodex`. No Nanocodex library crate depends on
or contains payment code. The CLI starts an in-process loopback WebSocket
adapter and gives its URL to the normal Nanocodex Responses configuration.
Consequently the existing persistent socket, typed stream processing, retry
policy, retained history, `previous_response_id`, and `store: false` behavior
remain unchanged.

```text
Nanocodex ResponsesService (unchanged)
               |
       ordinary OpenAI frames
               |
      CLI loopback WS adapter
               |
 alloy-transport-mpp application socket
               |
 canonical MPP frames + native TIP-1034 vouchers
               |
          mpp-proxy/mppx
               |
      OpenAI Responses WebSocket
```

The loopback adapter does not forward Nanocodex's OpenAI bearer credential.
It forwards only the Responses beta, cache/session identity, timing, and user
agent headers needed by the upstream proxy.

## Canonical paid socket

The generic application transport belongs in `alloy-transport-mpp`, below its
Alloy JSON-RPC adapter. It carries arbitrary UTF-8 application data and owns the
payment framing:

1. Convert the `ws:`/`wss:` endpoint to `http:`/`https:` and issue an HTTP GET
   probe.
2. Parse every `WWW-Authenticate: Payment` challenge and choose the first one
   supported by the configured `PaymentProvider`.
3. Upgrade the WebSocket and send
   `{"mpp":"authorization","authorization":"Payment …"}`.
4. Wait for `payment-receipt` before exposing the socket.
5. Wrap application data as `{"mpp":"message","data":"…"}` and unwrap the
   same server envelope.
6. Intercept `payment-need-voucher`, obtain the requested cumulative voucher,
   and send it as another in-band authorization.
7. On shutdown, request `payment-close-ready`, sign a descriptor-bearing native
   close credential for the receipt's exact `spent` amount, and wait for the
   final receipt. The client rejects a close amount above its locally signed
   voucher ceiling.
8. Keep receipts and payment errors out of the application stream.

Receipt payloads remain opaque JSON at this layer. That permits any
MPP-compatible provider or future receipt extension; only the CLI's Tempo
session wrapper understands native voucher requests.

The old `alloy-transport-mpp` implementation waits for a noncanonical
in-socket `challenge` frame and uses `type`/`credential` envelopes. Current MPP
uses the HTTP 402 probe and `mpp`/`authorization` envelopes. Nanocodex must use
the canonical application socket, not that legacy dialect.

## Native Tempo sessions

The CLI configures `mpp::TempoSessionProvider` with the selected private key,
RPC endpoint, and maximum deposit. The OpenAI proxy offers native v2 before
legacy v1. The v2 challenge uses:

- Moderato chain ID `42431` or Tempo mainnet chain ID `4217`;
- TIP-1034 reserve precompile
  `0x4d50500000000000000000000000000000000000`;
- cumulative off-chain vouchers;
- optional server fee sponsorship.

`TempoSessionProvider::voucher_credential` returns a signed credential for
in-band transports without performing an unrelated HTTP POST. Its existing SSE
helper delegates to that method and then performs the POST.

Native session dependencies require Rust 1.93, newer than Nanocodex's Rust
1.88 library baseline. The executable declares that higher MSRV while the MPP
integration stays out of the library crates and their dependency graph.

## OpenAI proxy

`mpp-proxy` adds `GET /v1/responses` as a paid WebSocket endpoint while keeping
the existing HTTP POST route. The GET without an upgrade is the HTTP payment
probe. The upgrade path:

- uses `mppx`/`tempo.Ws.serve` for canonical session authorization and voucher
  verification;
- opens an outbound WebSocket to `https://api.openai.com/v1/responses`;
- accepts only `response.create` application frames;
- incrementally tokenizes output text with `o200k_base`, retaining a conservative
  128-token suffix so later deltas cannot change an already charged boundary;
- calls `stream.charge(incremental_output_amount)` before releasing each output
  delta, causing mppx to request the next cumulative native session voucher;
- forwards the caller's request and OpenAI events byte-for-byte, so the
  Nanocodex wire stream and retained history remain unchanged;
- prices the terminal event from OpenAI's authoritative usage counters;
- accounts independently for uncached input, cached input, cache writes,
  visible and reasoning output, the 272K long-context tier, service tier, and
  supported paid tools;
- charges only the remaining difference before releasing the terminal event;
- allows one response in flight and resets that state only on an OpenAI
  terminal event;
- reuses the upstream WebSocket for every sequential response;
- caps queued upstream data at 64 MiB while a voucher is being obtained;
- observes mppx's cancellation signal so native session close cannot deadlock
  behind an idle application generator.

Output remains live: only a delta that makes a sufficiently old group of output
tokens stable pauses for payment, and that delta is released as soon as its
cumulative voucher is verified. The retained tokenizer suffix is charged during
terminal reconciliation. OpenAI does not stream input, cache-write, hidden
reasoning, paid-tool, or final long-context accounting as individual tokens, so
the authoritative terminal usage event reconciles those amounts without double
charging. The persistent WebSocket and retained response chain are unchanged.

For `gpt-5.6-sol`, standard short-context prices per million tokens are $5
uncached input, $0.50 cached input, $6.25 cache writes, and $30 output. The
resolver also contains the published Terra and Luna tables, Flex and Priority
tiers, long-context multipliers, and Web/File Search call fees. Amounts round
up to the proxy currency's one-micro-dollar atomic unit.

MPP sessions require a positive base tick, so the route advertises one atomic
unit. Every actual OpenAI request supplies its explicit dynamically calculated
amount; the base tick is not used as the request price.

Dynamic WebSocket charging requires `mppx`'s session controller to accept an
optional explicit amount. Calling `charge()` without an amount preserves the
existing fixed-tick behavior.

## CLI

Enable the adapter with:

```text
--mpp
--mpp-responses-websocket-url <ws-or-wss-url>
--tempo-private-key <key>                 # prefer TEMPO_PRIVATE_KEY
--tempo-rpc-url <url>
--mpp-max-deposit <atomic-units>
--mpp-api-key <key>                       # optional gated deployment key
```

These are global CLI flags. They select the same paid transport for both the
interactive TUI and the headless one-shot runner:

```text
nanocodex --mpp --prompt "say hello"
nanocodex run "say hello" --mpp
```

Both paths retain the adapter until the agent handle is dropped and then
perform the canonical signed session close. TUI teardown restores the terminal
before waiting for that network handshake.

The endpoint is configurable, so the same transport can call other compatible
MPP WebSocket services. A local proxy uses a service subdomain, for example
`ws://openai.localhost:8787/v1/responses`. Service discovery and arbitrary HTTP
services remain a separate caller/tool concern; they do not belong in
Nanocodex's model runtime.

## Validation

Required validation before merging:

- `mppx`: explicit dynamic charge reservation/commit test.
- `mpp-rs`: cumulative native voucher credential test and canonical
  probe/authorization/application-frame integration test.
- `mpp-proxy`: format, lint, typecheck, full unit suite, and an OpenAI WS bridge
  test.
- Nanocodex: rustfmt, Clippy with warnings denied, CLI tests, and unchanged
  library tests.
- Live: faucet-fund a fresh Moderato payer, open a native v2 channel against a
  local proxy, and complete two sequential real OpenAI Responses turns through
  one paid WebSocket without exposing either secret.
